# Changelog

All notable changes will be documented here. The project follows Semantic Versioning after the first public release.

## [Unreleased]

### Added

- Rust CLI and responsive terminal dashboard.
- Local profile, workspace, session, checkpoint, handoff, and activity state.
- Provider-neutral Agent/ModelProvider/Model domain types and SQLite v4 migrations.
- Codex, Claude Code, Gemini CLI, and OpenCode detection, isolated agent homes, auth orchestration, model preferences, guarded native resume, and handoff launch.
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
- Agent Workspace Handoff Format 1.1, checkpoint v2, ADRs, and verified five-agent capability documentation.
