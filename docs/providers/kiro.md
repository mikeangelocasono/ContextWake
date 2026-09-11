# Kiro CLI agent

Evidence was revalidated on 10 September 2026 against official Kiro CLI documentation. The QA host had Kiro IDE 1.0.337, not the separate `kiro-cli`, so no CLI behavior was locally claimed and no AgentDeck adapter is registered.

| Capability | Classification | AgentDeck status |
|---|---|---|
| Authentication/status | CONFIRMED IN DOCS | `kiro-cli login` and `whoami --format json`; not integrated |
| Profile isolation | CONFIRMED IN DOCS | `KIRO_HOME` is documented for independent profiles; not integrated |
| Session listing/resume | CONFIRMED IN DOCS | `--list-sessions`, `--resume`, and `--resume-id`; not integrated |
| Model listing | CONFIRMED IN DOCS | `--list-models --format json`; not integrated |
| Provider quota usage | UNSUPPORTED BY AGENTDECK | No quota value is displayed |
| Portable AWHF | CORE SUPPORTED | Format can be created/imported, but Kiro launch injection is not implemented |

## Official references

- [CLI command reference](https://kiro.dev/docs/cli/reference/cli-commands/)
- [Session management](https://kiro.dev/docs/cli/chat/session-management/)
- [Authentication](https://kiro.dev/docs/cli/authentication/)
- [Settings and `KIRO_HOME`](https://kiro.dev/docs/cli/reference/settings/)
