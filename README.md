<p align="center">
  <img src="docs/assets/contextwake-hero.svg" alt="ContextWake - One workspace. Any coding agent. Keep your context." width="100%">
</p>

<p align="center">
  <a href="https://github.com/mikeangelocasono/ContextWake/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/mikeangelocasono/ContextWake/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/mikeangelocasono/ContextWake/releases"><img alt="GitHub release" src="https://img.shields.io/github/v/release/mikeangelocasono/ContextWake?include_prereleases&sort=semver"></a>
  <a href="LICENSE"><img alt="Apache-2.0 license" src="https://img.shields.io/badge/license-Apache--2.0-6d8cff"></a>
  <img alt="Rust 1.88+" src="https://img.shields.io/badge/rust-1.88%2B-22b8cf">
</p>

<p align="center">
  <strong>One workspace. Any coding agent. Keep your context.</strong>
</p>

ContextWake is an open-source, terminal-native workspace and context-continuity manager for AI coding CLIs. It keeps profiles, workspaces, Git state, sessions, checkpoints, and portable handoffs explicit as you move between Codex CLI, Claude Code, OpenCode, Gemini CLI, and Kiro CLI.

ContextWake is local-first. It has no web dashboard, hosted control plane, telemetry pipeline, or cloud account service. Coding-agent credentials remain owned by each agent.

## Why ContextWake?

AI-assisted work is increasingly fragmented across agents, accounts, models, sessions, repositories, and branches. A provider may know its own conversation, but it does not necessarily know the state of another tool or identity.

ContextWake provides a local continuity layer around those agents:

- inspect the active profile, agent, model preference, workspace, and Git state;
- use a provider's native session resume only when the recorded identity is compatible;
- capture accessible project state in provider-neutral checkpoints;
- review and carry that state into a new agent session with a portable handoff;
- keep local activity distinct from unavailable provider quota or context metrics.

## ContextWake in action

![Real ContextWake TUI showing a profile, dirty Git workspace, and current checkpoint](docs/assets/contextwake-tui.png)

_Real 120 x 36 ContextWake TUI capture from a disposable dirty Git fixture. The local repository path was replaced with `demo-app`; no provider output or feature was fabricated._

## Features

- Keyboard-first Ratatui dashboard with wide, narrow, tiny-terminal, empty, warning, and error states.
- Provider-neutral agent adapter and graded capability system.
- Local profile metadata without plaintext passwords or copied access tokens.
- Git and non-Git workspace registry with branch, status, conflicts, ahead/behind, and diff summaries.
- Local session browser with search, archive, workspace/profile filters, and guarded native resume.
- Versioned checkpoints containing user-supplied objectives plus accessible workspace and Git evidence.
- Agent Workspace Handoff Format (AWHF) JSON and Markdown for portable context restoration.
- Transactional profile switching: target validation and artifact creation complete before activation.
- Context Guardian indicators based on local checkpoint age and observable Git drift.
- Privacy-safe diagnostics through `ctxwake doctor`.
- JSON CLI output, shell completions, SQLite schema migrations, and cross-platform automation.

## Supported AI coding agents

Support is deliberately capability-specific. "Partial" often means command construction and failure behavior are verified, while a disposable authenticated session test is still pending.

| Coding agent | Detection | Authentication | Sessions / native resume | Models | Portable handoff | Current status |
|---|---|---|---|---|---|---|
| OpenAI Codex CLI | Verified 0.154.0 on Windows | Verified; isolated profile signed out | Resume contract verified; authenticated resume pending | Selection verified | Verified | Partial |
| Claude Code | Verified 2.1.267 on Windows | Verified JSON status | Resume contract verified; authenticated resume pending | Hosted modes supported | Verified | Partial |
| OpenCode | Verified 1.18.25 on Windows | Credential-presence check | JSON listing verified; authenticated resume pending | Dynamic catalog verified | Verified | Partial |
| Gemini CLI | Verified 0.59.0 on Windows | Interactive; non-interactive status unavailable | Resume contract verified; authenticated resume pending | Selection verified | Verified | Partial |
| Kiro CLI | Verified 2.21.3 on Windows | JSON status verified | Real same-identity listing, sync, and resume verified | Dynamic catalog verified | Verified | Partial; isolated Windows identities unsupported |

See the [full compatibility matrix](docs/providers/compatibility.md) and the evidence for [Codex](docs/providers/codex.md), [Claude Code](docs/providers/claude.md), [OpenCode](docs/providers/opencode.md), [Gemini CLI](docs/providers/gemini.md), and [Kiro CLI](docs/providers/kiro.md).

## How context continuity works

ContextWake never treats cross-agent context restoration as conversation transfer.

### Native Resume

```text
same coding agent + same authorized profile + native provider session
                              |
                              v
                         Native Resume
```

The provider resumes its own session. ContextWake only offers this path when the local record, active profile, and adapter capability agree.

### Portable Handoff

```text
Codex / Personal
       |
       | switch to Claude / Work
       v
   Checkpoint  -->  AWHF handoff  -->  new Claude Code session
                                                |
                                                v
                                  Restored from Handoff
```

A handoff can contain objective, current task, completed work, decisions, constraints, changed files, Git summary, validation results, known issues, pending work, and trusted project instructions. It excludes credentials, hidden reasoning, chain-of-thought, and inaccessible provider state.

## Installation

### Windows x86_64

Download `contextwake-v0.1.0-alpha.1-windows-x86_64.zip` and `SHA256SUMS` from the [v0.1.0-alpha.1 prerelease](https://github.com/mikeangelocasono/ContextWake/releases/tag/v0.1.0-alpha.1), verify the archive, extract it, and run:

```powershell
Get-FileHash .\contextwake-v0.1.0-alpha.1-windows-x86_64.zip -Algorithm SHA256
Expand-Archive .\contextwake-v0.1.0-alpha.1-windows-x86_64.zip
.\contextwake-v0.1.0-alpha.1-windows-x86_64\ctxwake.exe doctor
```

The alpha executable is unsigned. Do not disable Defender or other platform protection; compare its hash with `SHA256SUMS`.

### Linux x86_64

Download `contextwake-v0.1.0-alpha.1-linux-x86_64.tar.gz` and `SHA256SUMS` from the prerelease, then:

```sh
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf contextwake-v0.1.0-alpha.1-linux-x86_64.tar.gz
./contextwake-v0.1.0-alpha.1-linux-x86_64/ctxwake doctor
```

### macOS

Build from source until a macOS artifact has completed the public release workflow. Automated CI is not physical-device TUI validation, signing, or notarization.

### Build from source

Install Rust 1.88 or newer and Git, then:

```sh
git clone https://github.com/mikeangelocasono/ContextWake.git
cd ContextWake
cargo build --release --locked
./target/release/ctxwake --version
```

On Windows, run `.\target\release\ctxwake.exe --version` instead. Coding-agent CLIs are optional unless you want to use their adapters.

## Quick Start

```sh
# Inspect the machine and current directory
ctxwake doctor
ctxwake status
ctxwake agent detect

# Register a legitimate local profile and this workspace
ctxwake profile add Personal --agent codex --model-provider openai
ctxwake workspace add .

# Capture accessible project state
ctxwake checkpoint create --objective "Continue the current implementation"
ctxwake checkpoint list

# Launch the TUI
ctxwake
```

Authenticate through the coding agent when needed:

```sh
ctxwake profile login personal
```

ContextWake initiates the provider-owned login flow; it does not ask you to paste a token into its configuration.

## Usage

The CLI and TUI use the same application services and SQLite state.

| Area | Useful commands |
|---|---|
| Status | `ctxwake status`, `ctxwake doctor --verbose` |
| Agents | `ctxwake agent list`, `ctxwake agent detect`, `ctxwake agent info codex` |
| Profiles | `ctxwake profile add`, `list`, `show`, `use`, `login`, `logout`, `remove` |
| Workspaces | `ctxwake workspace add`, `list`, `show`, `open`, `validate`, `remove` |
| Sessions | `ctxwake session sync`, `list`, `show`, `resume`, `archive` |
| Checkpoints | `ctxwake checkpoint create`, `list`, `show`, `export`, `delete` |
| Handoffs | `ctxwake handoff create`, `list`, `show`, `preview`, `export`, `import`, `continue` |
| Models | `ctxwake model list`, `ctxwake model select` |
| Configuration | `ctxwake config show`, `path`, `validate`, `set` |
| Automation | `ctxwake --json ...`, `ctxwake completion <shell>` |

Run `ctxwake <command> --help` for exact arguments. Use `--ascii` when the terminal cannot render Unicode status markers.

## Example workflow

```sh
# While Codex / Personal is active in the current project
ctxwake checkpoint create \
  --objective "Ship the authentication flow" \
  --task "Add refresh-token failure coverage" \
  --completed "Implemented token rotation" \
  --decision "Authentication errors return typed results" \
  --pending "Add integration tests"

# Switch explicitly; ContextWake creates continuity before activation
ctxwake profile use work --handoff \
  --objective "Continue the authentication flow" \
  --workspace .

# Review the generated package before starting a new provider session
ctxwake handoff list
ctxwake handoff preview <handoff-id>
ctxwake handoff continue <handoff-id>
```

The last command starts a **new** agent session restored from the handoff. It is not reported as Native Resume.

The repository also contains a [deterministic continuity demo](demo/README.md) that uses an explicitly labeled provider fixture and never presents mock interaction as a live provider response.

## TUI controls

| Key | Action |
|---|---|
| `G` | Home dashboard |
| `P` / `I` | Profiles / agent capabilities |
| `W` / `S` | Workspaces / sessions |
| `C` / `O` | Checkpoints / handoffs |
| `U` / `D` / `T` | Local activity / diagnostics / settings |
| `J`, `K`, arrows | Move selection |
| `Enter` | Open or confirm the current action |
| `/` | Search sessions |
| `R` | Refresh local and provider state |
| `Esc` | Back or cancel |
| `?` | Key reference |
| `Q` | Quit |

Identity changes show Git evidence and require confirmation. Handoff previews require a separate action before an agent is launched.

## Local data

`CONTEXTWAKE_HOME` overrides all platform paths and creates `config/`, `data/`, and `cache/` beneath the selected directory. Without an override, ContextWake uses native application directories:

| Platform | Configuration | Local state |
|---|---|---|
| Windows | `%APPDATA%\ContextWake\ContextWake\config` | `%LOCALAPPDATA%\ContextWake\ContextWake\data` |
| Linux | `$XDG_CONFIG_HOME/contextwake` | `$XDG_DATA_HOME/contextwake` |
| macOS | `~/Library/Application Support/dev.ContextWake.ContextWake` | Same application-support root |

Use `ctxwake config path` or `ctxwake doctor --verbose` to see the exact paths selected on a machine. Repository-local, non-secret metadata lives in `.contextwake/project.toml`; see [configuration](docs/configuration.md).

## Security and privacy

- Local-first storage and telemetry disabled by default.
- Provider-owned authentication; no ContextWake credential table or token export.
- Fixed executable/argument arrays rather than user-built shell commands.
- Bounded provider probes and sanitized terminal output.
- Strict, size-bounded handoff schemas with hashes, secret scanning, traversal checks, and symlink rejection.
- Repository validation commands remain untrusted until explicitly approved and never run on open.
- No automatic Git commit, stash, reset, push, or history mutation.
- Local activity is never labeled as provider credits, quota, balance, or token availability.

ContextWake is not designed to bypass provider quotas, rate limits, subscription restrictions, authentication controls, or provider Terms of Service.

Read the [security model](docs/security.md), [data classification](docs/data-classification.md), and [vulnerability reporting policy](SECURITY.md).

## Platform support

| Platform | Current evidence |
|---|---|
| Windows 11 x86_64 | Native build, 83 tests, release executable, TUI, and five-agent detection verified |
| Linux x86_64 | WSL build, 87 tests, Cargo package, audit, and extracted artifact verified |
| macOS | Workflow configured; public GitHub-hosted validation pending |

## Current limitations

- Authenticated native resume remains pending for Codex, Claude Code, OpenCode, and Gemini CLI.
- Kiro same-identity resume is verified, but its Windows OS credential is shared across `KIRO_HOME` roots; ContextWake does not advertise isolated Kiro profiles.
- Cross-agent artifact fidelity is verified; authenticated destination-agent comprehension still needs deliberately authorized disposable profiles.
- Current adapters do not expose reliable provider quota balances or context percentages.
- The TUI refresh path is synchronous; event-driven background refresh is planned.
- Windows artifacts are unsigned. macOS signing, notarization, hosted CI, and physical-device QA remain pending.

See [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md) for evidence and remaining work.

## Roadmap

- Complete authenticated provider resume and cross-agent comprehension QA.
- Add event-driven TUI refresh and richer model selection.
- Extend session ingestion where providers expose stable structured metadata.
- Add property/fuzz testing for handoff and provider parsers.
- Design a signed, versioned adapter SDK before considering third-party plugins.

## Architecture

```text
                          ContextWake
                              |
             +----------------+----------------+
             |                |                |
        Workspaces         Sessions         Profiles
             |                |                |
             +----------------+----------------+
                              |
                      Continuity Engine
                              |
                       AgentAdapter API
                              |
        +----------+----------+----------+----------+
        |          |          |          |          |
      Codex      Claude    OpenCode    Gemini      Kiro
```

Agents, model providers, models, profiles, workspaces, sessions, checkpoints, and handoffs remain separate domain objects. Read the [architecture](docs/architecture.md) and the [language/TUI](docs/adr/0001-language-and-tui-stack.md), [storage](docs/adr/0002-storage.md), and [adapter](docs/adr/0003-provider-adapter-model.md) decisions.

## Development and testing

Standard tests use disposable workspaces and mock/isolated provider boundaries. No paid account or provider credential is required.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo build --locked --workspace --release
cargo audit
cargo package --locked
```

See [development setup](docs/development.md) and [CONTRIBUTING.md](CONTRIBUTING.md). Contributions must preserve truthful capability reporting, explicit account switching, and the terminal-only product boundary.

## Security reporting

Please use GitHub's private security-advisory flow. Do not post credentials, private paths, raw authentication files, or unredacted diagnostics in a public issue. Details are in [SECURITY.md](SECURITY.md).

## Community

Please read the [Code of Conduct](CODE_OF_CONDUCT.md) before participating. Bug reports, feature proposals, and provider compatibility reports are welcome through the repository issue templates.

## License

Licensed under [Apache-2.0](LICENSE).
