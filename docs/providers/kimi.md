# Kimi Code CLI

Reviewed 14 September 2026 against the current `MoonshotAI/kimi-code` documentation and source. This adapter targets Kimi Code, not the older winding-down `kimi-cli` project or the Kimi web chatbot. `kimi` was not installed on the Windows QA host.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | Official Kimi Code CLI; canonical executable `kimi` |
| Tested version | NOT TESTED | No local installation was available |
| Installation detection | PARTIAL | Contract requires current Kimi Code version/help signatures; live binary pending |
| Authentication | UNKNOWN | Provider-owned `kimi login`; no safe credential-validating status command is documented, and ContextWake does not inspect secret-bearing configuration |
| Configuration | PARTIAL | Officially documented `KIMI_CODE_HOME` relocation; live isolation pending |
| Profile isolation | PARTIAL | The official layout supports separate roots; real multi-identity leakage testing is pending |
| Model selection | PARTIAL | Contract uses `--model <alias>`; model-provider JSON is intentionally not ingested because it may contain plaintext API keys |
| Session listing | PARTIAL | Contract-tested bounded parsing of documented `session_index.jsonl` and `state.json`; paths must remain inside the Kimi profile root |
| Native resume | PARTIAL | `--session <id>` is implemented; authenticated continuity challenge is pending |
| Non-interactive mode | PARTIAL | Documented `--prompt <prompt>`; live prompt pending |
| Structured output | PARTIAL | Documented `--output-format stream-json`; live output pending |
| ACP | PARTIAL | Documented and contract-tested `kimi acp` fixed-argv transport; live handshake pending |
| MCP | PARTIAL | Documented MCP configuration and ACP forwarding; live QA pending |
| Portable handoff | PARTIAL | The AWHF root is mounted into a new interactive session; the user must explicitly ask Kimi to read it because no safe interactive initial-prompt flag is documented |
| Windows / Linux / macOS | NOT TESTED | Cross-platform Rust contracts run without credentials; live CLI and authenticated QA pending |

## Credential safety

ContextWake creates only the isolated directory structure. It never creates, reads, copies, or stores Kimi credentials. Dynamic provider configuration is excluded from ingestion because API keys may be serialized there. Logout remains provider-interactive rather than simulated by deleting files.

## Privacy and limitations

Detection and local session-index reads do not upload project content. Explicit Kimi invocation may send context according to the selected Kimi/model-provider configuration. ContextWake does not start Kimi's optional web service. Provider quota and context percentage are not exposed.

Official references: [Kimi Code repository](https://github.com/MoonshotAI/kimi-code), [command reference](https://github.com/MoonshotAI/kimi-code/blob/main/docs/en/reference/kimi-command.md), [sessions guide](https://github.com/MoonshotAI/kimi-code/blob/main/docs/en/guides/sessions.md), [data locations](https://github.com/MoonshotAI/kimi-code/blob/main/docs/en/configuration/data-locations.md), and [ACP reference](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-acp).
