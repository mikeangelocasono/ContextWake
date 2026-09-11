# Changelog

All notable changes will be documented here. The project follows Semantic Versioning after the first public release.

## [Unreleased]

No unreleased changes.

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
