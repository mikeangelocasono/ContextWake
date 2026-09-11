# Gemini CLI agent

Evidence was revalidated on 11 September 2026 against the official Gemini CLI documentation and source. Gemini CLI was not installed on the QA host, so the adapter is registered as partial and its process contract is tested with fixtures rather than claimed as a live integration.

| Capability | Classification | AgentDeck status |
|---|---|---|
| Detect executable/version | CONFIRMED IN DOCS/SOURCE | Implemented with a bounded version probe; live binary QA pending |
| Authentication flow | PARTIAL | Provider-owned interactive selector can be launched; resulting status remains truthfully `Unknown` |
| Authentication status/logout | UNKNOWN/UNSUPPORTED | No supported non-interactive status or logout command was verified |
| Session listing | PARTIAL | `--list-sessions` is documented but human-readable; AgentDeck does not parse it |
| Native continuation | CONFIRMED | Same-profile `--resume <UUID>` command implemented; live authenticated QA pending |
| Model selection | CONFIRMED | `--model`; implemented |
| Account-specific model catalog | UNSUPPORTED | Documented aliases are not treated as a live account catalog |
| Profile isolation | CONFIRMED IN DOCS, QA PENDING | Dedicated `GEMINI_CLI_HOME`; OAuth/keychain isolation still needs cross-platform validation |
| Programmatic output | CONFIRMED | Headless JSON/stream-JSON is documented but not needed for interactive P0 launch |
| Provider quota usage | UNSUPPORTED BY AGENTDECK | No quota value is displayed |
| Portable AWHF | IMPLEMENTED | Starts with `--prompt-interactive`, an explicit include directory, extensions disabled, and default approval mode |

AgentDeck creates `<profile-home>/.gemini/settings.json` containing only a schema reference and `general.enableAutoUpdate=false`. It stores no API keys. `GEMINI_CLI_HOME` points at the UUID-scoped profile root because Gemini appends its own `.gemini` directory.

Gemini launches through AgentDeck use `--extensions none` and `--approval-mode default`. AgentDeck does not pass `--yolo` or `--skip-trust`. This reduces extension and implicit-approval risk without claiming to sandbox Gemini CLI.

On Windows, the official npm package exposes `dist/index.js`. AgentDeck searches for a trusted PATH `node.exe` and that package entry point, never an npm batch wrapper. A native `gemini.exe` remains preferred if one exists. Repository-local launchers are rejected.

## Official references

- [Gemini CLI reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md)
- [Session management](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md)
- [Configuration and `GEMINI_CLI_HOME`](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md)
- [Authentication](https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/authentication.mdx)
- [Official npm package manifest](https://github.com/google-gemini/gemini-cli/blob/main/packages/cli/package.json)
