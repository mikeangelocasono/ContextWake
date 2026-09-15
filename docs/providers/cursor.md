# Cursor CLI

Reviewed and locally probed on Windows 14 September 2026. Cursor updated during QA from `2026.07.23-e383d2b` to `2026.09.10-fd3934a`; final detection and native-resume evidence use the latter.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | Cursor CLI; current official executable `agent` |
| Installation detection | VERIFIED | Requires Cursor-specific version/help output. ContextWake rejected an unrelated xAI-signed `agent.exe` found on PATH |
| Authentication | VERIFIED | `agent status` was exercised; the adapter retains only `signed_in`, `signed_out`, or `unknown`, not account identity |
| Configuration | VERIFIED | `CURSOR_CONFIG_DIR` is set for profile-scoped settings |
| Profile isolation | UNSUPPORTED identities / PARTIAL settings | Provider-owned authentication remained available across a fresh `CURSOR_CONFIG_DIR`, so ContextWake does not advertise isolated Cursor identities |
| Model selection / discovery | VERIFIED | `--model` plus the live account-specific `--list-models` catalog; model families remain separate from the Cursor agent identity |
| Session listing | UNSUPPORTED | `agent ls` is an interactive picker, not a stable machine-readable listing interface |
| Native resume | VERIFIED | A normal interactive session established four known facts; ContextWake resumed the exact chat and a fresh question recovered all 4/4. Windows verbatim workspace paths are normalized only at the Cursor argument boundary |
| Non-interactive mode | VERIFIED | Authenticated `--print --mode ask --model auto` was exercised in the disposable fixture without modifications |
| Structured output | PARTIAL | JSON and stream-JSON interfaces verified; live structured prompt pending |
| ACP | PARTIAL | Live `agent acp` interface and fixed-argv transport verified; protocol handshake pending |
| MCP | PARTIAL | `agent mcp` and ACP MCP forwarding are documented; server interaction pending |
| Workspace / worktree | PARTIAL | Workspace/add-directory interface is explicit; no live worktree or handoff launch was run |
| Cloud handoff | UNSUPPORTED by ContextWake | Cursor cloud workers are never started implicitly |
| Portable handoff | VERIFIED | Cursor reconstructed a Copilot-attributed AWHF handoff at 7/7 plus exact branch, HEAD, and 1/1/1 Git counts. A Cursor-attributed handoff was then reconstructed by OpenCode at 7/7 |
| Windows | VERIFIED | Detection, version, auth boundary, dynamic models, and signature collision defense |
| Linux / macOS | NOT TESTED | Cross-platform Rust contracts run in CI; live Cursor CLI not tested on those hosts |

## Generic executable safety

The name `agent` is never sufficient evidence. On Windows ContextWake may safely launch Cursor's documented local bundle directly through its bundled Node executable, avoiding `cmd`, PowerShell, or a shell wrapper. Every candidate must still pass Cursor-specific help and version signatures and may not resolve inside the current untrusted repository.

## Privacy and limitations

Detection and model/status probes send no workspace content. An explicit Cursor launch can transmit context according to Cursor's own behavior. ContextWake does not invoke cloud-worker functionality. Provider quota is not exposed, and interactive session history is not scraped.

Official references: [CLI overview](https://docs.cursor.com/en/cli/overview), [CLI parameters](https://docs.cursor.com/en/cli/reference/parameters), [output formats](https://docs.cursor.com/en/cli/reference/output-format), [configuration](https://prod.cursor.com/docs/cli/reference/configuration), [ACP](https://prod.cursor.com/docs/cli/acp), and [MCP](https://prod.cursor.com/docs/cli/mcp).
