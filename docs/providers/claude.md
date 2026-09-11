# Claude Code agent

Evidence was revalidated on 10 September 2026 against official Anthropic documentation and installed Claude Code 2.1.267.

| Capability | Classification | ContextWake behavior |
|---|---|---|
| Detect executable/version | CONFIRMED / IMPLEMENTED | Bounded `claude --version` probe |
| Authentication status | CONFIRMED / IMPLEMENTED | `claude auth status --json` in the selected home |
| Login/logout | CONFIRMED / IMPLEMENTED | Provider-owned interactive commands |
| Profile isolation | PARTIALLY CONFIRMED / IMPLEMENTED | Sets documented `CLAUDE_CONFIG_DIR`; macOS Keychain isolation still needs platform QA |
| Session resume | CONFIRMED / IMPLEMENTED | Same-profile `claude --resume <id-or-name>` |
| Session listing | PARTIALLY CONFIRMED | Native picker/transcripts exist; ContextWake does not parse them in P0 |
| Model selection | CONFIRMED / IMPLEMENTED | Passes `--model` only when explicitly configured |
| Multiple backends | CONFIRMED | Anthropic, Bedrock, Vertex AI, and Foundry modes are documented |
| Context reporting | PARTIAL | In-session `/context`; no stable external probe used |
| Provider quota usage | UNSUPPORTED BY ADAPTER | No quota value is displayed |
| Handoff launch | IMPLEMENTED | New session receives a reviewed AWHF directory through fixed argv |

Claude Code owns its credentials. ContextWake never reads `.credentials.json` or Keychain entries.

## Official references

- [CLI reference](https://code.claude.com/docs/en/cli-usage)
- [Session management](https://code.claude.com/docs/en/sessions)
- [Environment variables](https://code.claude.com/docs/en/env-vars)
- [Authentication and credential management](https://code.claude.com/docs/en/team)
