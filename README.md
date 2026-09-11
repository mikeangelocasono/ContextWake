# AgentDeck

> Provisional project and binary names; no trademark or package-name availability is claimed.

**One workspace. Any coding agent. Keep your context.**

AgentDeck is a local-first, terminal-native continuity manager for AI coding CLIs. It keeps coding agents, model backends, profiles, workspaces, Git state, sessions, checkpoints, and portable handoffs explicit without pretending to transfer hidden model context.

There is no web application, hosted dashboard, SaaS control plane, or browser management portal. Provider-owned browser authentication may still open when the selected coding CLI requires it.

## Working alpha

The current milestone includes:

- one Rust `adeck` binary with a keyboard-first Ratatui interface and structured CLI;
- distinct Agent, Model Provider, Model, Profile, Workspace, Session, Checkpoint, Handoff, and Git Snapshot models;
- real Codex, Claude Code, OpenCode, and conservative Gemini adapters with bounded probes, isolated profile homes, explicit model selection, native same-profile resume commands, and AWHF new-session launch;
- a compile-time capability registry that refuses unsupported agents instead of inventing behavior;
- Git and non-Git workspaces, dirty-state evidence, local sessions, checkpoint v2, and Agent Workspace Handoff Format 1.1;
- transactional profile switching: validate and build continuity first, activate last;
- hash-verified, bounded, secret-scanned handoff import/export;
- explicit-only trusted project validation using fixed executable/argument arrays;
- local Context Guardian indicators and local activity separated from unavailable provider quota data;
- `adeck doctor`, JSON output, strict errors, migrations, tests, and cross-platform CI.

Gemini CLI is registered from verified official interfaces but lacks installed-binary QA on this host. Kiro CLI is researched but is **not** registered. OpenCode session/model commands are integrated; its profile isolation remains experimental pending cross-platform credential QA. See the [verified compatibility matrix](docs/providers/compatibility.md) and [implementation status](IMPLEMENTATION_STATUS.md).

## Build

Prerequisites are Rust 1.88+, Git, and—optionally—Codex CLI, Claude Code, Gemini CLI, or OpenCode. Windows builds need Visual Studio C++ Build Tools.

```powershell
cargo build --release
.\target\release\adeck.exe --version
```

macOS/Linux:

```sh
cargo build --release
./target/release/adeck --version
```

Prebuilt/signed release packages and automatic updates are not published yet.

## Quick start

```text
# Inspect real local state
adeck status
adeck agent detect
adeck doctor --verbose

# Create legitimate identity/configuration boundaries
adeck profile add Personal --agent codex --model-provider openai
adeck profile add Work --agent claude --model-provider anthropic --model sonnet
adeck profile add Gemini --agent gemini --model-provider google --model flash
adeck profile add OSS --agent opencode
adeck profile login personal

# Register a Git or ordinary folder
adeck workspace add .
adeck workspace open CODEXDECK

# Import supported native session metadata for the active profile/workspace
adeck session sync --workspace .

# Record accessible project state
adeck checkpoint create --objective "Continue the continuity workflow"
adeck checkpoint list

# Create, inspect, and export a portable handoff
adeck handoff create <checkpoint-id>
adeck handoff preview <handoff-id>
adeck handoff export <handoff-id> ./handoff

# Explicitly switch; artifact creation completes before activation
adeck profile use work --handoff --objective "Continue in Claude Code"

# Review, then start a NEW session from the handoff
adeck handoff continue <handoff-id>

# Launch the terminal UI
adeck
```

The first profile becomes active. Press `Tab` in first-run/profile creation to choose a registered coding agent. Profiles store metadata and an agent configuration-home reference, never passwords or copied tokens. Authentication remains owned by the coding agent.

Model configuration is profile-aware:

```text
adeck model list
adeck model select opus --provider anthropic --profile work
adeck model select opencode/big-pickle --profile oss
```

Static adapters accept only declared provider IDs. OpenCode accepts its documented custom-provider IDs and validates selections against `opencode models` before persistence. Model catalogs remain unavailable when an agent has no reliable machine-readable command.

## Continuity semantics

AgentDeck exposes two distinct outcomes:

1. **Native resume** — the active agent resumes a recorded provider session only when that session is native-capable and its agent/profile identity matches.
2. **Restored from handoff** — AgentDeck starts a new agent session with an explicit, reviewable project-state package.

A handoff can contain objective/task notes, decisions, changed filenames, Git summary, validation status, pending work, and trusted project instructions. It cannot contain hidden reasoning, inaccessible conversation state, or credentials. A restored session is never labeled “resumed.”

## Repository configuration

Optional `.agentdeck/project.toml` supports project defaults and fixed-argv validation declarations. It is ignored until the workspace is explicitly trusted, and commands never run merely because a repository was opened.

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

- Telemetry and update checks are off by default; no AgentDeck cloud service exists.
- Provider auth files are never parsed, copied, logged, checkpointed, or exported.
- Git/provider commands use argument arrays rather than shell concatenation.
- Repository/provider text is stripped of terminal controls before rendering.
- Imported handoffs have strict schemas, path/size/media bounds, content hashes, symlink rejection, and secret scanning.
- Account/agent changes require explicit user intent. AgentDeck does not rotate identities based on limits and is not intended to bypass quotas, rate limits, subscriptions, authentication, or provider terms.
- “LOCAL ACTIVITY” is never presented as provider credits, quota, or remaining tokens.

Read the [security model](docs/security.md) and [vulnerability policy](SECURITY.md).

## Documentation

- [Architecture](docs/architecture.md) and [ADRs](docs/adr/0001-language-and-tui-stack.md)
- [PRD review](docs/prd-review.md) and [implementation status](IMPLEMENTATION_STATUS.md)
- [Agent compatibility](docs/providers/compatibility.md): [Codex](docs/providers/codex.md), [Claude](docs/providers/claude.md), [Gemini](docs/providers/gemini.md), [OpenCode](docs/providers/opencode.md), [Kiro](docs/providers/kiro.md)
- [AWHF handoff format](docs/handoff-format.md)
- [Data classification](docs/data-classification.md)
- [Development](docs/development.md) and [contributing](CONTRIBUTING.md)

## License

Apache-2.0. See [LICENSE](LICENSE).
