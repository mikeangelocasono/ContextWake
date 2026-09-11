# Architecture

ContextWake is a terminal-only Rust application. The CLI and TUI call the same application services; neither owns business logic.

```text
CLI router / Ratatui TUI
          |
Application service boundary
          |
          +-- Agent registry and capability contracts
          +-- Profile and transactional switch coordinator
          +-- Workspace and Git inspector
          +-- Local session index
          +-- Checkpoint and AWHF handoff engines
          +-- Diagnostics and local activity
          |
AgentAdapter (compile-time, capability-gated)
          +-- CodexAdapter
          +-- ClaudeAdapter
          +-- GeminiAdapter
          +-- OpenCodeAdapter

SQLite schema v4         Versioned local artifacts
Agent-owned homes        No ContextWake credential database
```

## Domain separation

The following remain independent types and persisted fields:

- `CodingAgent`: the coding CLI, such as Codex or Claude Code.
- `ModelProvider`: an inference backend exposed through an agent, such as OpenAI, Anthropic, Bedrock, Vertex, Ollama, or LM Studio.
- `AgentModel`: a provider-specific model identifier and non-authoritative cost classification.
- `Profile`: a local label plus agent-owned configuration-home boundary.
- `Workspace`, `Session`, `Checkpoint`, `HandoffRecord`, and `GitSnapshot`: provider-neutral continuity state.

An agent can expose several model providers. A provider is never assumed to be an agent, and a profile does not contain credentials.

## Agent adapter boundary

`src/provider` is the only module that constructs coding-agent commands or sets agent-specific environment variables. `AgentAdapter` requires explicit capability reporting and uses fixed argument arrays. The registry is compile-time in V1; arbitrary downloaded plugins are intentionally excluded until signing, compatibility, and sandboxing have a design.

Codex, Claude Code, Gemini CLI, OpenCode, and Kiro CLI are implemented adapters. OpenCode and Kiro exercise dynamic model catalogs and provider-owned JSON session discovery. Gemini remains deliberately partial where no machine-readable auth/session surface exists. Kiro's Windows credential is explicitly treated as shared even though `KIRO_HOME` isolates settings and sessions.

## Continuity transaction

```text
validate target profile and installed agent
             |
capture accessible workspace/Git state
             |
create checkpoint + handoff when requested
             |
activate target profile (final mutation)
             |
explicit native resume OR explicit new session from handoff
```

If target validation or artifact creation fails, active-profile state is unchanged. Native resume is allowed only for a recorded native-capable session whose agent and profile match the active profile. Restored-from-handoff is a separate session state and message.

## Storage

SQLite schema v4 stores queryable relationships, normalized agent/model-provider/model fields, and provider session titles. Migrations preserve legacy v1/v2/v3 records. Checkpoint v2 and AWHF 1.2 bodies are inspectable JSON/Markdown files. AWHF 1.0/1.1 and checkpoint v1 remain readable.

Agent-owned authentication stays inside profile-specific `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `GEMINI_CLI_HOME`, or OpenCode XDG/config roots. ContextWake does not parse, copy, export, or put credentials in SQLite.

## Trust and responsiveness

Git and non-interactive agent probes are bounded subprocesses. Repository validation is fixed-argv, trusted-workspace gated, explicit-only, and has null standard streams plus a timeout. Imported artifacts and rendered terminal text are untrusted.

The current TUI performs synchronous refreshes, so background event-driven refresh remains P1. Interactive provider processes start only after ContextWake restores the ordinary terminal.

See the [language/TUI](adr/0001-language-and-tui-stack.md), [storage](adr/0002-storage.md), and [adapter](adr/0003-provider-adapter-model.md) decisions.

Field-level persistence rules are in the [data classification](data-classification.md).
