# ContextWake

> The name passed a practical package/product collision audit; this is not formal trademark clearance. Recheck before public publication.

**One workspace. Any coding agent. Keep your context.**

ContextWake is a local-first, terminal-native continuity manager for AI coding CLIs. It keeps coding agents, model backends, profiles, workspaces, Git state, sessions, checkpoints, and portable handoffs explicit without pretending to transfer hidden model context.

There is no web application, hosted dashboard, SaaS control plane, or browser management portal. Provider-owned browser authentication may still open when the selected coding CLI requires it.

## Working alpha

The current milestone includes:

- one Rust `ctxwake` binary with a keyboard-first Ratatui interface and structured CLI;
- distinct Agent, Model Provider, Model, Profile, Workspace, Session, Checkpoint, Handoff, and Git Snapshot models;
- real Codex, Claude Code, OpenCode, Gemini CLI, and Kiro CLI adapters with bounded probes, truthful isolation capabilities, explicit model selection, guarded native resume, and AWHF new-session launch;
- a compile-time capability registry that refuses unsupported agents instead of inventing behavior;
- Git and non-Git workspaces, dirty-state evidence, local sessions, checkpoint v2, and Agent Workspace Handoff Format 1.2;
- transactional profile switching: validate and build continuity first, activate last;
- hash-verified, bounded, secret-scanned handoff import/export;
- explicit-only trusted project validation using fixed executable/argument arrays;
- local Context Guardian indicators and local activity separated from unavailable provider quota data;
- `ctxwake doctor`, JSON output, strict errors, migrations, tests, and cross-platform CI.

Gemini CLI 0.59.0 detection was validated on Windows without authenticating. Kiro CLI 2.21.3 is registered and its JSON sessions/models plus same-identity native resume were live-tested; its Windows OS credential is shared, so Kiro multiple-profile isolation is explicitly unsupported. OpenCode session/model commands are integrated; its profile isolation remains experimental pending cross-platform credential QA. See the [verified compatibility matrix](docs/providers/compatibility.md) and [implementation status](IMPLEMENTATION_STATUS.md).

## Build

Prerequisites are Rust 1.88+, Git, and optionally Codex CLI, Claude Code, Gemini CLI, OpenCode, or Kiro CLI. Windows builds need Visual Studio C++ Build Tools.

```powershell
cargo build --release
.\target\release\ctxwake.exe --version
```

macOS/Linux:

```sh
cargo build --release
./target/release/ctxwake --version
```

Prebuilt/signed release packages and automatic updates are not published yet.

## Quick start

```text
# Inspect real local state
ctxwake status
ctxwake agent detect
ctxwake doctor --verbose

# Create legitimate identity/configuration boundaries
ctxwake profile add Personal --agent codex --model-provider openai
ctxwake profile add Work --agent claude --model-provider anthropic --model sonnet
ctxwake profile add Gemini --agent gemini --model-provider google --model flash
ctxwake profile add OSS --agent opencode
ctxwake profile login personal

# Register a Git or ordinary folder
ctxwake workspace add .
ctxwake workspace open CONTEXTWAKE

# Import supported native session metadata for the active profile/workspace
ctxwake session sync --workspace .

# Record accessible project state
ctxwake checkpoint create --objective "Continue the continuity workflow"
ctxwake checkpoint list

# Create, inspect, and export a portable handoff
ctxwake handoff create <checkpoint-id>
ctxwake handoff preview <handoff-id>
ctxwake handoff export <handoff-id> ./handoff

# Explicitly switch; artifact creation completes before activation
ctxwake profile use work --handoff --objective "Continue in Claude Code"

# Review, then start a NEW session from the handoff
ctxwake handoff continue <handoff-id>

# Launch the terminal UI
ctxwake
```

The first profile becomes active. Press `Tab` in first-run/profile creation to choose a registered coding agent. Profiles store metadata and an agent configuration-home reference, never passwords or copied tokens. Authentication remains owned by the coding agent.

Model configuration is profile-aware:

```text
ctxwake model list
ctxwake model select opus --provider anthropic --profile work
ctxwake model select opencode/big-pickle --profile oss
```

Static adapters accept only declared provider IDs. OpenCode accepts its documented custom-provider IDs and validates selections against `opencode models` before persistence. Model catalogs remain unavailable when an agent has no reliable machine-readable command.

## Continuity semantics

ContextWake exposes two distinct outcomes:

1. **Native resume** — the active agent resumes a recorded provider session only when that session is native-capable and its agent/profile identity matches.
2. **Restored from handoff** — ContextWake starts a new agent session with an explicit, reviewable project-state package.

A handoff can contain objective/task notes, decisions, changed filenames, Git summary, validation status, pending work, and trusted project instructions. It cannot contain hidden reasoning, inaccessible conversation state, or credentials. A restored session is never labeled “resumed.”

## Repository configuration

Optional `.contextwake/project.toml` supports project defaults and fixed-argv validation declarations. It is ignored until the workspace is explicitly trusted, and commands never run merely because a repository was opened.

```toml
schema_version = 1
name = "Example"
preferred_agent = "codex"
preferred_model_provider = "openai"
preferred_profile = "personal"

[[validation]]
executable = "cargo"
args = ["test", "--locked"]
```

See [configuration](docs/configuration.md).

## Security and privacy

- Telemetry and update checks are off by default; no ContextWake cloud service exists.
- Provider auth files are never parsed, copied, logged, checkpointed, or exported.
- Git/provider commands use argument arrays rather than shell concatenation.
- Repository/provider text is stripped of terminal controls before rendering.
- Imported handoffs have strict schemas, path/size/media bounds, content hashes, symlink rejection, and secret scanning.
- Account/agent changes require explicit user intent. ContextWake does not rotate identities based on limits and is not intended to bypass quotas, rate limits, subscriptions, authentication, or provider terms.
- “LOCAL ACTIVITY” is never presented as provider credits, quota, or remaining tokens.

Read the [security model](docs/security.md) and [vulnerability policy](SECURITY.md).

## Documentation

- [Architecture](docs/architecture.md), [stack ADR](docs/adr/0001-language-and-tui-stack.md), and [naming ADR](docs/adr/0004-project-naming.md)
- [PRD review](docs/prd-review.md) and [implementation status](IMPLEMENTATION_STATUS.md)
- [Agent compatibility](docs/providers/compatibility.md): [Codex](docs/providers/codex.md), [Claude](docs/providers/claude.md), [Gemini](docs/providers/gemini.md), [OpenCode](docs/providers/opencode.md), [Kiro](docs/providers/kiro.md)
- [AWHF handoff format](docs/handoff-format.md)
- [Data classification](docs/data-classification.md)
- [Development](docs/development.md) and [contributing](CONTRIBUTING.md)

## License

Apache-2.0. See [LICENSE](LICENSE).
