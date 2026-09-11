# Configuration

Run `adeck config path` to see the platform-specific global file. Set `AGENTDECK_HOME` to an isolated root for tests or portable development.
Use `adeck config set --ascii true` (and the other documented flags) for atomic,
validated changes without opening an editor. The command exposes no credential
or provider-home settings.

Default global configuration:

```toml
schema_version = 1
telemetry = false
retention_days = 30
ascii = false
update_check = false
git_timeout_ms = 2000
validation_timeout_ms = 120000

[agent_experimental]
codex_app_server = false
```

Optional repository metadata lives at `.agentdeck/project.toml`:

```toml
schema_version = 1
name = "Example"
preferred_agent = "codex"
preferred_model_provider = "openai"
preferred_model = "gpt-5.3-codex"
preferred_profile = "work"
instructions_file = "AGENTS.md"

[[validation]]
executable = "cargo"
args = ["test", "--locked"]
```

Project configuration is ignored until the workspace is explicitly trusted. Validation declarations never run solely because a repository was opened.
For a trusted workspace, `instructions_file` must remain inside the workspace,
must be a regular non-symlink UTF-8 file, and is limited to 64 KiB. Its
secret-redacted, terminal-safe contents are captured in new checkpoints.
Run `adeck workspace validate` to execute them explicitly, or pass `--validate`
to `adeck checkpoint create` to execute them and capture redacted status-only
results. Standard input and output are disconnected, and each command has the
configured timeout. AgentDeck does not invoke a shell or persist command output.

Precedence is CLI flags, environment overrides, trusted project configuration, selected profile metadata, global configuration, and built-in defaults. Project configuration may not control provider credential paths, telemetry, or update channels.

Agent executable overrides are process-local and must point to trusted binaries:

```text
AGENTDECK_CODEX_BIN=/absolute/path/to/codex
AGENTDECK_CLAUDE_BIN=/absolute/path/to/claude
AGENTDECK_GEMINI_BIN=/absolute/path/to/gemini
AGENTDECK_OPENCODE_BIN=/absolute/path/to/opencode
```

AgentDeck never discovers an executable from a repository-local path by itself. Legacy project key `preferred_provider` and global table `[provider_experimental]` remain readable, but new files serialize `preferred_agent` and `[agent_experimental]`.
