# Security Model

AgentDeck assumes the local operating-system user account is trusted. A fully compromised host is out of scope. Repositories, branch names, provider output, handoff packages, symlinks, and exported paths are not trusted.

## Implemented controls

- Credentials stay agent-owned in profile-scoped `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `GEMINI_CLI_HOME`, or OpenCode XDG/config roots; ordinary AgentDeck config and SQLite have no secret fields.
- Provider and Git processes receive fixed argument arrays. Repository content is never shell-interpolated.
- Implicit agent discovery ignores relative PATH entries and binaries located inside the current workspace. Explicit `AGENTDECK_*_BIN` overrides remain a user-owned trust decision.
- ANSI CSI/OSC sequences and control bytes are stripped before untrusted terminal text is rendered.
- Provider session titles are terminal-sanitized, secret-redacted, capped at 200 characters, and retained locally.
- Checkpoint and handoff text passes through a secret scanner with redaction counts.
- Trusted project instructions are containment checked, limited to 64 KiB, required to be UTF-8 regular non-symlink files, and sanitized before checkpoint capture.
- Portable handoffs omit the absolute workspace path and include a one-way path fingerprint.
- Managed artifact reads use canonical containment checks and reject symlink payloads.
- Imported handoffs have strict schemas and bounded size/count/media, require the untrusted-command flag, are scanned for secrets/control data, and verify every content hash before copying.
- Writes use temporary files, explicit overwrite flags, and symlink rejection.
- Managed Unix application directories are forced to mode `0700`; managed directory, config, database, and artifact symlinks are rejected where applicable.
- Repository commands are structured as executable plus args, require workspace trust and an explicit validate action, use bounded execution with null standard streams, and are never automatically run.
- Native session resume is blocked when either the recorded coding agent or profile differs from the active profile.
- Telemetry and experimental provider protocols are disabled by default.
- Error output is sanitized and secret-redacted before display; ordinary provider probe failures are reduced to bounded, single-line summaries instead of exposing raw stack traces.
- Windows agent launch resolves native Codex/Claude/OpenCode executables and Gemini's official Node entry point rather than passing handoff-related arguments through batch wrappers.

## Sensitive data

Workspace paths, objectives, decisions, filenames, branch names, and validation command names can be sensitive. They stay local unless the user explicitly exports a checkpoint/handoff. Validation subprocess output is not captured or persisted. The user must preview exports before sharing them.

AgentDeck never intentionally persists passwords, API keys, access/refresh tokens, bearer tokens, provider auth-file contents, complete environments, raw transcripts, or hidden model reasoning.

The alpha does not persist application logs. Human and JSON errors are centrally
sanitized and secret-redacted before they reach the terminal. Future diagnostic
logging must remain opt-in, bounded, and pass through the same redaction policy.

## Reporting

Do not open a public issue containing credentials, private repository names, workspace paths, or raw provider diagnostics. Follow [SECURITY.md](../SECURITY.md) for private reporting.
