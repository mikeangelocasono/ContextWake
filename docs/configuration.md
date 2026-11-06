# Configuration

Run `ctx config path` to see the platform-specific global file. Set `CONTEXTWAKE_HOME` to an isolated root for tests or portable development.
It must be an absolute path. A relative value is refused, because it would
resolve against whichever directory `ctx` runs in, and coding agents, which run
inside the workspace, would resolve their configuration homes against the
workspace instead. An empty value is treated as unset.
Use `ctx config set --ascii true` (and the other documented flags) for atomic,
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

`telemetry`, `update_check`, `retention_days` and
`[agent_experimental] codex_app_server` are reserved. They are accepted so that
existing files keep loading, but no feature uses them yet: ContextWake sends no
telemetry, never checks for updates, never deletes checkpoints, handoffs or
session records on its own (delete them explicitly, for example with
`ctx checkpoint delete <id> --yes`), and has no Codex app-server integration.
`ctx config set` refuses to switch them on, and `ctx doctor` lists any that
`config.toml` sets.

## State location and permissions

On Unix, ContextWake restricts its state directories to your account (mode
0700). On Windows it does not change permissions. The default per-user
locations under `%APPDATA%` and `%LOCALAPPDATA%` are private to your account,
but a `CONTEXTWAKE_HOME` outside your user profile (for example
`D:\contextwake` on a data volume) inherits the permissions of the folder it is
created in, which often let every local user read it. Agent login state,
checkpoints and handoffs live there, and `ctx doctor` warns about such a
location. The check resolves short (8.3) names, junctions and symbolic links
first, so a junction inside your profile that points to another volume is
reported too. Keep the root inside your user profile, or remove the inherited
access, for example from Command Prompt:

```text
icacls "D:\contextwake" /inheritance:r /grant:r "%USERDOMAIN%\%USERNAME%:(OI)(CI)F" "*S-1-5-18:(OI)(CI)F"
```

The state database stores workspace paths in their conventional form
(`C:\Users\...`, not the `\\?\C:\...` form that Windows path canonicalization
returns) and records absolute agent-home, checkpoint and handoff paths. When
the data directory moves (for example the legacy AgentDeck directory move),
those entries are pointed at the same files in the new location the next time
the database opens.

## Profiles and agent sign-in

Each profile records which configuration home its coding agent uses (`home_mode`
in `ctx profile show --json`, also shown by `ctx profile list`, `ctx status` and
`ctx doctor`):

* **Existing sign-in** (`agent_default`): the agent's own default home, so the
  profile uses the login you already have. For Claude Code that is
  `CLAUDE_CONFIG_DIR` if it is set to an absolute path, else `%USERPROFILE%\.claude`;
  for Codex CLI `CODEX_HOME`, else `%USERPROFILE%\.codex`; for Gemini CLI
  `GEMINI_CLI_HOME`, else your user profile (state under `.gemini`). ContextWake
  records that path, never sets the agent's home variable when it launches the
  agent, and never writes into, initializes or deletes that directory. The
  sign-in state is read from the agent's own status command when the profile is
  created (best effort; `unknown` when it cannot be checked).
* **Separate sign-in** (`isolated`): a ContextWake-managed home under the data
  directory (`agent-homes/<agent>/<profile id>`), passed to the agent through
  its home variable. Sign in once with `ctx profile login <name>`.

`ctx profile add NAME --agent A` uses the existing sign-in when the agent
supports it and no other profile uses that home yet (agent homes are unique, so
at most one profile per agent uses the existing sign-in); otherwise it creates
a separate sign-in and says why. `--existing-login` demands the existing
sign-in and fails when it is unavailable; `--isolated` always creates a
separate one. The TUI's new-profile form offers the same choice (Up/Down
toggles it). The library entry points that predate this choice
(`Application::create_profile*`) keep creating separate sign-ins.

For an existing-login profile, `ctx profile login` and `ctx profile logout`
act on your normal agent login (the one you also use outside ContextWake); both
print a notice first, and logout requires `--yes`. `ctx profile remove` only
deletes ContextWake's metadata and never touches either kind of home. If the
agent's home variable changes after the profile was created, `ctx doctor` warns
that the recorded home no longer matches; inside an agent that ContextWake
launched for a profile with a separate sign-in, it reports that comparison as
not checked instead. Profiles created before state schema
v6 keep separate sign-ins.

## Workspaces and project configuration

Commands that take a workspace reference accept its ID, its path in any
spelling that names the same directory (`.`, a relative path or a trailing
separator, and on Windows also `/` separators, other letter case or the `\\?\`
form), or its display name. Letter case is decided by the filesystem: in a
case-sensitive folder, `Repo` and `repo` are different directories and so
different workspaces, and registering one never inherits the other's trust.
A display name shared by several workspaces is
refused with the candidates instead of picking one; retry with an ID or path.
A provider session ID recorded for several local sessions is refused the same
way.

Optional repository metadata lives at `.contextwake/project.toml`:

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
Run `ctx workspace validate` to execute them explicitly, or pass `--validate`
to `ctx checkpoint create` to execute them and capture redacted status-only
results. Standard input and output are disconnected, and each command has the
configured timeout. ContextWake does not invoke a shell or persist command output.

For a trusted workspace, `name` becomes the workspace display name. `preferred_agent`,
`preferred_model_provider`, and `preferred_model` are recorded on the workspace (see
`ctx workspace show`) and `preferred_profile` is accepted, but all four are informational:
ContextWake never switches profiles because of them, and agent launches use the active
profile's agent, model backend, and model. If a trusted `project.toml` cannot be used (for
example, it contains a key this version does not know), ContextWake ignores it with a
warning; `ctx workspace validate` and `ctx checkpoint create --validate` report the error.

ContextWake settings come from CLI flags, then environment overrides (`CONTEXTWAKE_HOME`,
`CONTEXTWAKE_<AGENT>_BIN`), then `config.toml`, then built-in defaults. Project configuration
may not control provider credential paths, telemetry, or update channels.

Agent executable overrides are process-local and must point to trusted binaries:

```text
CONTEXTWAKE_CODEX_BIN=/absolute/path/to/codex
CONTEXTWAKE_CLAUDE_BIN=/absolute/path/to/claude
CONTEXTWAKE_COPILOT_BIN=/absolute/path/to/copilot
CONTEXTWAKE_CURSOR_BIN=/absolute/path/to/cursor-agent
CONTEXTWAKE_GEMINI_BIN=/absolute/path/to/gemini
CONTEXTWAKE_GROK_BIN=/absolute/path/to/grok
CONTEXTWAKE_KIMI_BIN=/absolute/path/to/kimi
CONTEXTWAKE_KIRO_BIN=/absolute/path/to/kiro-cli
CONTEXTWAKE_OPENCODE_BIN=/absolute/path/to/opencode
```

ContextWake never discovers an executable from a repository-local path by itself. During the pre-release rename, `AGENTDECK_HOME` (which follows the same absolute-path rule as `CONTEXTWAKE_HOME`), `AGENTDECK_<AGENT>_BIN`, and `.agentdeck/project.toml` remain readable at lower precedence. Default legacy OS state directories are moved only when the corresponding ContextWake destination does not exist; existing destinations are never overwritten. Legacy project key `preferred_provider` and global table `[provider_experimental]` also remain readable, while new files serialize `preferred_agent` and `[agent_experimental]`.
