# Kimi Code CLI

Reviewed 14 September 2026 against current `MoonshotAI/kimi-code` documentation, source, and Kimi Code 0.42.0 on Windows. This adapter targets the current TypeScript Kimi Code CLI, not the deprecated Python `kimi-cli` or the Kimi web chatbot.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | Official Kimi Code CLI; canonical executable `kimi` |
| Tested version | VERIFIED | Kimi Code 0.42.0, installed from the official `@moonshot-ai/kimi-code` package |
| Installation detection | VERIFIED | Version/help signatures were exercised. On Windows, ContextWake safely invokes the official JavaScript entry point using an absolute Node executable and never executes the npm command shim through a shell |
| Authentication | PENDING AUTH | Provider-owned `kimi login`; the isolated profile has no configured provider, and ContextWake does not inspect secret-bearing configuration |
| Configuration | VERIFIED | Officially documented `KIMI_CODE_HOME` relocation and an isolated empty configuration root were exercised |
| Profile isolation | PARTIAL | The official layout supports separate roots; real multi-identity leakage testing is pending |
| Model selection | PARTIAL | Contract uses `--model <alias>`; model-provider JSON is intentionally not ingested because it may contain plaintext API keys |
| Session listing | PARTIAL | Live `kimi session list --json` returned an empty valid catalog; bounded/contained parsing of `session_index.jsonl` and `state.json` is contract-tested |
| Native resume | PENDING AUTH | `--session <id>` is implemented; authenticated continuity challenge is pending |
| Non-interactive mode | PARTIAL | Documented `--prompt <prompt>`; live prompt pending |
| Structured output | PARTIAL | Documented `--output-format stream-json`; live output pending |
| ACP | PARTIAL | Documented and contract-tested `kimi acp` fixed-argv transport; live handshake pending |
| MCP | PARTIAL | Documented MCP configuration and ACP forwarding; live QA pending |
| Portable handoff | PARTIAL | The AWHF root is mounted into a new interactive session; the user must explicitly ask Kimi to read it because no safe interactive initial-prompt flag is documented |
| Windows | VERIFIED | Official package version/help, signature detection, isolated home, doctor, provider boundary, and empty JSON session listing |
| Linux / macOS | PARTIAL | Cross-platform Rust contracts run in CI; live CLI and authenticated QA are pending |

## Credential safety

ContextWake creates only the isolated directory structure. It never creates, reads, copies, or stores Kimi credentials. Dynamic provider configuration is excluded from ingestion because API keys may be serialized there. Logout remains provider-interactive rather than simulated by deleting files.

## Privacy and limitations

Detection and local session-index reads do not upload project content. Explicit Kimi invocation may send context according to the selected Kimi/model-provider configuration. ContextWake does not start Kimi's optional web service. Provider quota and context percentage are not exposed.

Official references: [Kimi Code repository](https://github.com/MoonshotAI/kimi-code), [command reference](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command), [sessions guide](https://www.kimi.com/code/docs/en/kimi-code-cli/guides/sessions), [data locations](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/data-locations.html), and [environment variables](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/env-vars).
