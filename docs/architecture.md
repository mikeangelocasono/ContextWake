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
          +-- GitHubCopilotAdapter
          +-- CursorAdapter
          +-- GeminiAdapter
          +-- OpenCodeAdapter
          +-- KiroAdapter
          +-- KimiAdapter
          +-- GrokAdapter
          |
          +-- optional fixed-argv ACP transport

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

Nine adapters are registered without provider-name branching in generic application code: Codex, Claude Code, GitHub Copilot CLI, Cursor CLI, Gemini CLI, OpenCode, Kiro CLI, Kimi Code, and Grok Build. OpenCode, Cursor, Kiro, and Grok exercise dynamic model catalogs where supported. OpenCode/Kiro consume structured session output; Copilot, Kimi, and Grok read only bounded documented metadata files. Cursor's interactive session picker is not scraped. Kiro and Cursor Windows credentials are explicitly treated as shared where real QA disproved identity isolation.

The registry stores trait objects in stable display order and resolves aliases through each adapter. Discovery uses bounded groups of three probes so several Node-based CLIs cannot starve each other's timeouts. Results retain registry order. The TUI runs one discovery set on a worker, shares those results with Doctor, animates while probes run, remains navigable during refresh, and safely joins the worker after every provider child has been reaped.

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

Agent-owned authentication stays in provider-managed storage. ContextWake supplies documented profile roots such as `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `COPILOT_HOME`, `CURSOR_CONFIG_DIR`, `GEMINI_CLI_HOME`, OpenCode XDG roots, `KIMI_CODE_HOME`, and `GROK_HOME`, but advertises identity isolation only where behavior is proven. It does not copy, export, or put credentials in SQLite.

## Trust and responsiveness

Git and non-interactive agent probes are bounded subprocesses. Repository validation is fixed-argv, trusted-workspace gated, explicit-only, and has null standard streams plus a timeout. Imported artifacts and rendered terminal text are untrusted.

Initial provider discovery and manual refresh use a background worker with bounded parallel probes. The render/input loop stays active, local mutations refresh immediately, and provider state follows asynchronously. Exit waits for bounded probes to kill/reap their children, preventing orphaned agent processes. Interactive provider processes start only after ContextWake restores the ordinary terminal.

## TUI interaction contract

The interface is an operator console rather than a splash screen: active
identity, workspace/Git evidence, and the next continuity action have priority.
Graphite surfaces use one restrained cyan accent and amber warnings; every
status also has explicit text so color is never the only signal. Mnemonic keys
open the primary screens, identity changes and destructive actions require
confirmation, and narrow or tiny terminals collapse to an essential one-column
view with a clear size warning.

See the [language/TUI](adr/0001-language-and-tui-stack.md), [storage](adr/0002-storage.md), [adapter](adr/0003-provider-adapter-model.md), and [ACP transport](adr/0006-acp-provider-transport.md) decisions.

Field-level persistence rules are in the [data classification](data-classification.md).
