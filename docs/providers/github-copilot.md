# GitHub Copilot CLI

Reviewed 14 September 2026 against the current GitHub documentation. ContextWake's adapter is contract-tested; `copilot` was not installed on the Windows QA host, so authenticated behavior is not marked verified.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | GitHub Copilot CLI; canonical executable `copilot` |
| Tested version | NOT TESTED | No local installation was available |
| Installation detection | PARTIAL | Contract-tested against both `copilot --version` and a Copilot-specific help signature; a same-named unrelated program is rejected; live CLI pending |
| Authentication | PARTIAL | Provider-owned `copilot login`; ContextWake does not spend a request merely to infer status and never stores OAuth material |
| Configuration | PARTIAL | Officially documented `COPILOT_HOME` relocation; live isolation pending |
| Profile isolation | PARTIAL | State roots can be separated with `COPILOT_HOME`; separation of multiple GitHub identities in the OS credential store is not proven |
| Model selection | PARTIAL | `--model` is passed as a fixed contract-tested argument; account-specific model discovery remains in Copilot's interactive picker |
| Session listing | PARTIAL | Contract-tested bounded read of `session.start` metadata from documented `COPILOT_HOME/session-state/*/events.jsonl`; prompts and credentials are not ingested |
| Native resume | PARTIAL | `--resume=<id>` is implemented; an authenticated continuity challenge has not been run |
| Non-interactive mode | PARTIAL | Documented `-p <prompt>`; no paid live prompt used |
| Structured output | PARTIAL | Documented JSONL via `--output-format=json`; live output pending |
| ACP | PARTIAL | Native `--acp` transport exists, with current provider limitations; ContextWake adds `--no-remote --no-remote-export` |
| MCP | PARTIAL | Copilot CLI officially exposes MCP configuration/commands; live QA pending |
| Portable handoff | PARTIAL | Contract starts a new local session with an explicit AWHF path; remote session behavior is disabled; live launch pending |
| Windows / Linux / macOS | NOT TESTED | Cross-platform Rust contract tests require no Copilot credentials; live CLI QA is pending on all three |

## Authentication and organization policy

Copilot owns its OAuth/device-code flow. ContextWake creates no GitHub credential record and never copies tokens between profile roots. Organization policy, entitlement, or region failures are reported only when Copilot returns an unambiguous diagnostic; a generic failure is not relabeled as policy-disabled.

## Privacy and limitations

Detection, version probing, and local session-metadata discovery do not send repository content. Starting Copilot with a handoff may transmit material Copilot chooses to read under the user's Copilot terms and settings. ContextWake neither starts remote sessions nor exports sessions remotely. Provider quota, credits, and reset time are not exposed.

Official references: [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), [programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference), [configuration directory](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference), [authentication](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli), and [multiple sessions](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/work-with-multiple-sessions).
