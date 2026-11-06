# Changelog

All notable changes will be documented here. The project follows Semantic Versioning after the first public release.

## [Unreleased]

### Added

- Cross-agent continuity for Claude Code, Codex CLI and Gemini CLI:
  `ctx profile use <profile> --handoff --launch` and `ctx handoff continue`
  resume the destination profile's own session in the workspace when the
  agent's native resume is verified (Claude Code `--resume`, `codex resume`)
  and deliver the handoff into it, otherwise start a new session; `--new-session`
  forces a new one. The TUI handoff preview shows the same decision.
- Delivery verification: after the agent exits, ContextWake finds the session
  the launch used in the agent's own local store and checks its transcript for
  the launch marker. "Restored from Handoff" and "Native Resume + Handoff" are
  reported only when the marker is found; otherwise ContextWake says plainly that
  delivery could not be confirmed.
- Profiles that use the agent's existing sign-in (`--existing-login`, the
  default when available): ContextWake never sets the agent's home variable,
  never writes into that home and never deletes it. `--isolated` keeps a
  separately signed-in, ContextWake-managed home. Store schema v6.
- Conversation excerpts: checkpoints (schema v3) store a bounded, redacted
  excerpt of the source agent's recent typed requests and latest answer from
  its local transcript (never tool output, reasoning or injected context);
  switch handoffs carry it to the next agent. Off with
  `capture_conversation_excerpts = false` or `--no-conversation`.
- A bounded launch prompt (at most 12,000 characters, secret-scanned) built by
  ContextWake: objective, current work, pending and completed tasks, decisions,
  known issues, the conversation excerpt, Git state, changed files and the path
  of the full `context.md`.

### Fixed

- Claude Code launches lost the handoff prompt: the variadic `--add-dir`
  swallowed it. Options now end with `--` before the prompt.
- Workspace detection no longer saves checkpoints to the wrong project, never
  silently switches workspace, refuses broad roots such as the home directory
  without `--workspace`, and treats verbatim (`\\?\`), case and separator
  variants of a Windows path as the same directory.
- Agent discovery ignores executables planted in the current repository,
  never runs `.cmd`/`.bat` wrappers, resolves npm shims to the native binary,
  bounds probe output and kills timed-out probe process trees.
- Git runs with repository-controlled programs disabled (hooks, fsmonitor,
  filters, external diff and textconv, submodule recursion) and bounded waits.
- Secret redaction covers whole secret values, private keys and more credential
  formats in checkpoints, handoffs, prompts and terminal output; hidden terminal
  control characters are stripped.
- Session sync no longer unarchives sessions or overstates resume capability;
  unreadable checkpoints and missing workspaces degrade to warnings instead of
  blocking commands.
- The Windows installer preserves unexpanded `%VARIABLE%` PATH entries and the
  registry value type, broadcasts the environment change, and updates in place;
  release builds link the C runtime statically.
- TUI stability on Windows: AltGr characters can be typed, pasted text and
  type-ahead no longer trigger commands, errors and missing workspaces no longer
  end the TUI, slow agent probes no longer block input, and the terminal state
  is restored on every exit path.

## [0.1.0-alpha.3] - 2026-09-16

### Added

- Checksum-verifying, user-local PowerShell and POSIX shell installers for the
  existing GitHub Release archives.
- State-preserving uninstall scripts and cross-platform installer CI covering
  fresh installation, reinstall, PATH idempotency, failure handling, and both
  `ctx` and `ctxwake`.

### Changed

- Made direct terminal installation the primary README path; Rust is now
  required only when building ContextWake from source.
- Simplified the public repository presentation and documented the Copilot-to-
  Codex portable-handoff flow.

## [0.1.0-alpha.2] - 2026-09-14

### Added

- GitHub Copilot CLI, Cursor CLI, Kimi Code, and Grok Build adapters with conservative evidence-based capabilities and portable AWHF launch paths.
- Optional fixed-argument ACP transports for Copilot, Cursor, Kimi, Grok, and OpenCode.
- Bounded provider session-metadata readers for Copilot, Kimi, and Grok, plus live Cursor and Grok model discovery.
- Deterministic seven-hop cross-agent CSV continuity fixture and nine-adapter portable-handoff contract coverage.
- Provider candidate review, individual provider guides, and contributor instructions for adding adapters safely.
- Release QA matrix and reproducible seven-field destination comprehension scoring.

### Changed

- Replaced the fixed provider registry with an ordered trait-object registry, adapter-owned aliases, shared safe probes, and bounded three-at-a-time discovery.
- Expanded the doctor, first-run detection, and TUI to nine agents; the TUI can filter installed, authenticated, resume-capable, or ACP-capable agents and refresh provider state without blocking input/rendering.
- Removed redundant TUI/Doctor provider probes and the implicit Codex fallback when no profile is configured.
- Expanded capability reporting for non-interactive mode, structured output, ACP, MCP, portable handoff, and separately classified cloud handoff.
- The canonical ContextWake command is now `ctx`; alpha release archives also
  include the shared-implementation `ctxwake` compatibility executable.
- Existing `.contextwake` project metadata, `CONTEXTWAKE_HOME`, and SQLite
  state remain compatible. The immutable `v0.1.0-alpha.1` release is unchanged.
- Live validation now covers GitHub Copilot named-session resume, Copilot-to-Cursor and Cursor-to-OpenCode 7/7 handoffs, and an evidence-preserving partial Codex-to-Copilot handoff.

### Security

- Generic executable names require vendor-specific signatures and repository-local candidates are rejected.
- Provider output and metadata are bounded and sanitized; provider credentials remain provider-owned and are never copied into SQLite.
- Copilot remote session/export flags and Cursor cloud workers are disabled in ContextWake launch paths.
- Provider-native session references reject path separators before a coding-agent process starts.

### Fixed

- Clarified the Windows archive extraction destination in the public installation instructions.
- Detect the official GitHub Copilot and Kimi Code npm distributions safely on Windows without executing npm command shims through a shell.
- Bound Copilot version reporting to the version line instead of retaining updater guidance.
- Normalize Windows verbatim workspace paths at the Cursor CLI boundary so provider-native resume can load persisted chats correctly.

## [0.1.0-alpha.1] - 2026-09-11

### Added

- Rust CLI and responsive terminal dashboard.
- Local profile, workspace, session, checkpoint, handoff, and activity state.
- Provider-neutral Agent/ModelProvider/Model domain types and SQLite v4 migrations.
- Codex, Claude Code, Gemini CLI, OpenCode, and Kiro CLI detection, profile-scoped agent homes where supported, auth orchestration, model preferences, guarded native resume, and handoff launch.
- Conservative Gemini adapter with `GEMINI_CLI_HOME`, unknown auth status, extensions disabled, default approvals, native resume, and interactive AWHF launch.
- OpenCode JSON session synchronization, dynamic provider/model catalog validation, custom/local backend support, and qualified model normalization.
- Git snapshots, deterministic handoffs, integrity hashes, secret redaction, and diagnostics.
- Strict, bounded handoff import/export with full content-integrity validation.
- SQLite v1-to-v2 migration for portable handoff records and v2-to-v3 normalized agent/model fields.
- Atomic SQLite v3-to-v4 migration for provider session titles.
- Explicit trusted validation execution with timeouts and redacted checkpoint results.
- Bounded trusted project-instruction capture and local Context Guardian indicators.
- Session filtering/search, transactional workspace pointers, and guarded TUI resume/continue actions.
- Atomic non-secret global configuration mutation and bounded provider health probes with single-line, redacted failure summaries.
- Cross-platform CI and contributor/security documentation.
- Deterministic Cargo source-package allowlist and Linux package verification, excluding the PRD and local build/QA artifacts.
- Agent Workspace Handoff Format 1.2 with explicit destination and portable-continuity semantics, checkpoint v2, ADRs, and verified five-agent capability documentation.
- Real Windows Gemini CLI 0.59.0 detection and unauthenticated-boundary QA.
- Kiro CLI 2.21.3 JSON authentication/session/model integration and real same-identity session synchronization/resume QA.
- Three directed Codex/Claude/OpenCode portable artifact flows preserving narrative, Git, validation, and project-instruction state.
- Public ContextWake landing page with original project artwork, a real sanitized TUI capture, provider-report issue template, and three-platform GitHub Actions validation.
- GitHub Actions release archives for Windows x86_64, Linux x86_64, and macOS arm64 with a generated SHA-256 manifest.

### Changed

- Renamed the project, crate, binary, configuration directory, diagnostics, and schemas from the conflicting AgentDeck identity to provider-neutral ContextWake/`ctxwake`.
- Retained lower-precedence legacy environment variables and `.agentdeck/project.toml`, plus safe one-time default state-directory migration without overwriting an existing ContextWake destination.

### Security

- Kiro multiple-profile support is disabled because real Windows QA showed that `KIRO_HOME` does not isolate its OS credential.
- Database startup rejects duplicate or unsupported schema metadata and migration rollback/idempotence are regression-tested.
- Release archives contain an explicit file set and receive SHA-256 manifests; Windows and macOS binaries remain truthfully unsigned/unnotarized.

### Provider support

- Kiro CLI session/model discovery and same-identity native resume are the only authenticated native-session behaviors verified end to end in this milestone.
- Codex, Claude Code, OpenCode, and Gemini CLI native commands remain partial pending disposable authenticated-profile QA.
- Cross-agent continuity uses portable handoffs and never claims native transcript transfer.

### Known limitations

- Public alpha binaries remain unsigned, macOS is not notarized, and no macOS physical-device QA has been performed.
- Provider context utilization and quota balances are not exposed by current adapters.
- Cross-agent artifact fidelity is verified; target-model comprehension still requires deliberately authorized disposable provider profiles.
