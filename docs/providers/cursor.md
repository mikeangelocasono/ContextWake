# Cursor CLI

Reviewed and locally probed on Windows 14 September 2026. The tested Cursor CLI version was `2026.07.23-e383d2b`.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | Cursor CLI; current official executable `agent` |
| Installation detection | VERIFIED | Requires Cursor-specific version/help output. ContextWake rejected an unrelated xAI-signed `agent.exe` found on PATH |
| Authentication | VERIFIED | `agent status` was exercised; the adapter retains only `signed_in`, `signed_out`, or `unknown`, not account identity |
| Configuration | VERIFIED | `CURSOR_CONFIG_DIR` is set for profile-scoped settings |
| Profile isolation | UNSUPPORTED | A fresh config root remained signed in during Windows QA, proving multiple-identity isolation is unavailable; scoped settings remain partial |
| Model selection / discovery | VERIFIED | `--model` plus the live account-specific `--list-models` catalog; model families remain separate from the Cursor agent identity |
| Session listing | UNSUPPORTED | `agent ls` is an interactive picker, not a stable machine-readable listing interface |
| Native resume | PARTIAL | `--resume <chat-id>` is implemented; end-to-end continuity challenge not run |
| Non-interactive mode | PARTIAL | `--print` interface verified from the live CLI; no paid prompt used |
| Structured output | PARTIAL | JSON and stream-JSON interfaces verified; live structured prompt pending |
| ACP | PARTIAL | Live `agent acp` interface and fixed-argv transport verified; protocol handshake pending |
| MCP | PARTIAL | `agent mcp` and ACP MCP forwarding are documented; server interaction pending |
| Workspace / worktree | PARTIAL | Workspace/add-directory interface is explicit; no live worktree or handoff launch was run |
| Cloud handoff | UNSUPPORTED by ContextWake | Cursor cloud workers are never started implicitly |
| Portable handoff | PARTIAL | Contract starts a new local Cursor session with the AWHF directory as an additional workspace root; live comprehension pending |
| Windows | VERIFIED | Detection, version, auth boundary, dynamic models, and signature collision defense |
| Linux / macOS | NOT TESTED | Cross-platform Rust contracts run in CI; live Cursor CLI not tested on those hosts |

## Generic executable safety

The name `agent` is never sufficient evidence. On Windows ContextWake may safely launch Cursor's documented local bundle directly through its bundled Node executable, avoiding `cmd`, PowerShell, or a shell wrapper. Every candidate must still pass Cursor-specific help and version signatures and may not resolve inside the current untrusted repository.

## Privacy and limitations

Detection and model/status probes send no workspace content. An explicit Cursor launch can transmit context according to Cursor's own behavior. ContextWake does not invoke cloud-worker functionality. Provider quota is not exposed, and interactive session history is not scraped.

Official references: [CLI overview](https://docs.cursor.com/en/cli/overview), [CLI parameters](https://docs.cursor.com/en/cli/reference/parameters), [output formats](https://docs.cursor.com/en/cli/reference/output-format), [configuration](https://prod.cursor.com/docs/cli/reference/configuration), [ACP](https://prod.cursor.com/docs/cli/acp), and [MCP](https://prod.cursor.com/docs/cli/mcp).
