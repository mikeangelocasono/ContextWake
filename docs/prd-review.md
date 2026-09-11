# PRD review and feasibility record

Reviewed in full on 10 September 2026. The terminal-native core, local state model, adapter architecture, SQLite recommendation, Git awareness, and checkpoint/handoff design are feasible and mutually reinforcing.

## Binding product corrections

- AgentDeck has no web application, dashboard, hosted control plane, SaaS portal, or browser account switcher. A coding agent may open its own browser authentication flow.
- A Coding Agent and a Model Provider are distinct. Codex, Claude Code, Gemini CLI, OpenCode, and Kiro are agents; OpenAI, Anthropic, Google, Bedrock, Vertex, Ollama, and similar systems are model backends.
- “Profile” is AgentDeck’s identity/configuration boundary. A Codex `--profile` only layers configuration and is not an account boundary.
- Native resume and new-session restoration from a handoff are separate, user-visible outcomes.
- No quota routing, limit-triggered identity rotation, auth bypass, fabricated usage, or fake context percentage is in scope.
- AgentDeck/`adeck` remain provisional. No trademark or package availability is claimed.

## Provider-dependent feasibility

| Requirement | Classification | Implementation decision |
|---|---|---|
| Codex executable/version/auth | CONFIRMED | Implemented with bounded stable commands |
| Codex same-profile resume | CONFIRMED | Implemented; requires recorded native session metadata |
| Codex cross-profile ownership | NOT SUPPORTED AS A PROMISE | Block native transfer; use AWHF |
| Codex model selection/local backends | CONFIRMED | Model preference and backend metadata implemented |
| Claude executable/version/auth | CONFIRMED | Implemented with bounded commands |
| Claude `CLAUDE_CONFIG_DIR` isolation | PARTIALLY CONFIRMED | Implemented; macOS Keychain behavior still needs platform QA |
| Claude same-profile resume/model flag | CONFIRMED | Implemented |
| Gemini profile/auth/model/session behavior | PARTIALLY CONFIRMED IN DOCS/SOURCE | `GEMINI_CLI_HOME`, model selection, resume, and AWHF launch implemented; auth status and machine session listing unavailable; live binary QA pending |
| OpenCode sessions/models/providers | CONFIRMED IN DOCS, SOURCE, AND LOCAL HELP | Adapter implemented with JSON session sync, dynamic model validation, native resume command, and AWHF launch; authenticated resume QA pending |
| Kiro auth/sessions/models/`KIRO_HOME` | CONFIRMED IN DOCS | Adapter not implemented; only Kiro IDE was installed locally |
| Stable provider context utilization | PARTIAL OR UNAVAILABLE | Never estimated; local continuity indicators are labeled |
| Provider quota balances | UNSUPPORTED BY CURRENT ADAPTERS | Hidden; local activity is separate |
| Portable checkpoint/handoff | PROPOSED / IMPLEMENTED | Checkpoint v2 and provider-neutral AWHF 1.1 |
| Repository validation | PROPOSED / IMPLEMENTED SAFELY | Trusted-workspace, fixed-argv, explicit-only, timeout bounded |

The detailed evidence is in the [compatibility matrix](providers/compatibility.md) and provider documents.

## Architecture decision

Rust was retained because the repository already had a sound passing foundation and the product benefits from one native binary, typed security boundaries, predictable subprocess construction, and Ratatui’s testable terminal buffers. The accepted decisions are recorded in [`docs/adr`](adr/0001-language-and-tui-stack.md).

SQLite schema v4 stores normalized agent/model-provider/model metadata, provider session titles, and transactional relationships. Checkpoint and handoff bodies remain inspectable files. Provider credentials are neither an AgentDeck data entity nor a database field.

## Consistency findings

- Continuity—not multi-account storage—is the signature workflow. The shared application service validates the target agent, creates artifacts before activation, and is called by both CLI and TUI.
- Capability flags require graded support states because agents expose fundamentally different surfaces.
- Portable handoff checkpoint IDs cannot be trusted as local foreign keys. Imports use a nil local association and require an explicit workspace before launch.
- Repository configuration and imported handoffs are untrusted documents, never executable automation.
- Signed update installation cannot exist safely until a real release channel and signing policy exist, so no fake update command is exposed.
- Dynamic third-party plugins would introduce arbitrary code execution and are deferred.

## Remaining feasibility work

Disposable authenticated native-resume QA, Gemini installed-binary QA, macOS Keychain isolation, Windows signed release testing, event-driven TUI refresh, property fuzzing, signed packaging, the Kiro adapter, and final naming clearance remain explicit follow-up work. None is presented as working functionality.
