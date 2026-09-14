# GitHub Copilot CLI

Reviewed 14 September 2026 against current GitHub documentation and GitHub Copilot CLI 1.0.83 on Windows. The official npm distribution, authenticated prompt mode, structured output, session discovery, and known-state native resume were exercised.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | GitHub Copilot CLI; canonical executable `copilot` |
| Tested version | VERIFIED | GitHub Copilot CLI 1.0.83 on Windows |
| Installation detection | VERIFIED | Requires both version and Copilot-specific help signatures. The Windows adapter safely resolves the official npm package's native executable without invoking its command shim or a shell |
| Authentication | VERIFIED request / PARTIAL status | An authenticated request succeeded through provider-owned GitHub credentials. Copilot exposes no dedicated non-spending status command, so ContextWake does not guess before a request |
| Configuration | VERIFIED | Officially documented `COPILOT_HOME` relocation was exercised with an isolated ContextWake profile |
| Profile isolation | PARTIAL | State roots can be separated with `COPILOT_HOME`; separation of multiple GitHub identities in the OS credential store is not proven |
| Model selection | PARTIAL | `--model` is passed as a fixed contract-tested argument; account-specific model discovery remains in Copilot's interactive picker |
| Session listing | VERIFIED | ContextWake synchronized a real named session by reading only bounded `session.start` metadata from `COPILOT_HOME/session-state/*/events.jsonl`; prompts and credentials were not ingested |
| Native resume | VERIFIED | ContextWake resumed a real named session; the resumed agent recalled objective, task, constraint, and known issue from the earlier provider session |
| Non-interactive mode | VERIFIED | Authenticated `-p <prompt>` requests were exercised with explicit read-only tools |
| Structured output | VERIFIED | JSONL events from `--output-format=json` were exercised and bounded |
| ACP | PARTIAL | Native `--acp` transport exists, with current provider limitations; ContextWake adds `--no-remote --no-remote-export` |
| MCP | PARTIAL | Copilot CLI officially exposes MCP configuration/commands; live QA pending |
| Portable handoff | PARTIAL | A real Codex-to-Copilot comprehension run scored 5/7, and a Copilot-attributed handoff was reconstructed by Cursor at 7/7. Remote session behavior remained disabled |
| Windows | VERIFIED | Official npm installation, signature detection, provider-owned authentication, JSONL prompt, session sync, and known-state native resume |
| Linux / macOS | PARTIAL | Cross-platform Rust contract tests run in CI; the Copilot CLI was not installed or authenticated on those runners |

## Authentication and organization policy

Copilot owns its OAuth/device-code flow. During QA, the provider reused its documented provider-owned GitHub CLI credential fallback; ContextWake created no GitHub credential record and copied no token into the isolated profile or SQLite. Organization policy, entitlement, or region failures are reported only when Copilot returns an unambiguous diagnostic; a generic failure is not relabeled as policy-disabled.

## Privacy and limitations

Detection, version probing, and local session-metadata discovery do not send repository content. Starting Copilot with a handoff may transmit material Copilot chooses to read under the user's Copilot terms and settings. ContextWake neither starts remote sessions nor exports sessions remotely. Provider quota, credits, and reset time are not exposed.

Official references: [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), [programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference), [configuration directory](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference), [authentication](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli), and [multiple sessions](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/work-with-multiple-sessions).
