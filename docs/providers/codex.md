# OpenAI Codex CLI agent

Evidence was revalidated on 10 September 2026 against official OpenAI documentation and installed `codex-cli 0.154.0`.

Non-interactive version and authentication probes have a five-second child
process timeout and bounded, redacted output. Interactive login, resume, and new
session processes deliberately remain under user control.

| Capability | Classification | ContextWake behavior |
|---|---|---|
| Detect executable/version | CONFIRMED | Runs `codex --version` |
| Auth status | CONFIRMED | Runs `codex login status` inside the selected `CODEX_HOME` |
| Login/logout | CONFIRMED | Hands the terminal to provider-owned commands |
| Config profiles (`--profile`) | CONFIRMED, CONFIG ONLY | Never treated as account separation |
| Dedicated `CODEX_HOME` | PARTIALLY CONFIRMED | Used as the profile process/state boundary |
| Native resume | CONFIRMED | Runs `codex resume <id> -C <workspace>` only for a compatible profile |
| Cross-profile resume | NOT SUPPORTED AS A PROMISE | Blocked when recorded profile identity differs; use handoff |
| Rich session listing | PARTIALLY CONFIRMED | Experimental app-server is disabled in P0 |
| Model selection | CONFIRMED / IMPLEMENTED | `--model`; agent and model-provider fields remain separate |
| Multiple/local backends | CONFIRMED | Custom `model_providers` plus Ollama/LM Studio OSS modes are documented |
| Account/plan/usage | UNSUPPORTED BY ADAPTER | No quota value is displayed |
| Context utilization/model reporting | PARTIALLY CONFIRMED | Hidden outside reliable provider events |

## Credential strategy

Each ContextWake profile owns a separate provider home. The adapter creates a non-secret `config.toml` with:

```toml
cli_auth_credentials_store = "file"
```

This selects a provider-supported, home-scoped credential file so identity boundaries do not depend on unverified keyring namespacing. Codex creates and owns `auth.json`; ContextWake does not read, copy, log, export, or store its contents. The containing application-data directory is user-scoped. OS-keyring-per-profile isolation remains a research item.

Removing ContextWake profile metadata does not log the provider out or delete the provider home. Logout is a separate, confirmed action.

## Official references

- [Codex developer command reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
- [Codex authentication](https://learn.chatgpt.com/docs/auth)
- [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
