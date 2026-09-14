# Grok Build

Reviewed and locally probed on Windows 14 September 2026. Grok Build updated during the release pass; final detection used `1.0.30 (04b7ffed98c6) [stable]`.

| Area | Status | ContextWake behavior and evidence |
|---|---|---|
| Product / executable | VERIFIED | Grok Build terminal coding agent; canonical executable `grok` |
| Installation detection | VERIFIED | `grok --version` plus Grok Build help signature; a generic `agent` is never accepted as Grok |
| Authentication | VERIFIED | `grok models` safely identified the isolated profile as signed out; provider-owned `grok login` and `grok logout` are used |
| Configuration | PARTIAL | `GROK_HOME` relocation is documented and isolated signed-out behavior was observed; two identities pending |
| Profile isolation | PARTIAL | Root relocation is documented and isolated signed-out behavior was observed; two authenticated identities were not tested |
| Model selection / discovery | VERIFIED | Fixed `--model` argument and live `grok models` catalog; Grok models selected inside other agents are not duplicate Grok Build agents |
| Session listing | PARTIAL | Contract-tested bounded parsing of documented `GROK_HOME/sessions/*/*/summary.json`; real session metadata pending |
| Native resume | PARTIAL | `--resume <id-or-title>` is implemented; authenticated continuity challenge is pending |
| Non-interactive mode | PARTIAL | Live CLI exposed `--single <prompt>`; no paid prompt used |
| Structured output | PARTIAL | Live CLI exposed JSON and streaming-JSON headless modes; live output pending |
| ACP | PARTIAL | Live CLI exposed `grok agent stdio` and the fixed-argv transport is tested; handshake pending |
| MCP | PARTIAL | Live CLI exposed `grok mcp` and ACP MCP support; server interaction pending |
| Context / usage | PARTIAL | In-session context and real per-session totals exist; subscription quota, credits, and reset times are not reported |
| Portable handoff | PARTIAL | Contract starts a new local Grok Build session with an explicit AWHF path; live comprehension pending |
| Windows | VERIFIED | Signed executable, version/help, auth boundary, model catalog, and adapter parsers |
| Linux / macOS | NOT TESTED | Cross-platform Rust contracts run in CI; live Grok Build not tested on those hosts |

## Privacy and limitations

Detection, version, model/auth probes, and summary metadata reads do not send repository contents. Explicit invocation may transmit workspace material under Grok Build's own behavior. ContextWake invokes the local CLI only and does not create remote dashboard tasks. No paid prompt or authenticated resume was used during this milestone.

Official references: [Grok Build repository](https://github.com/xai-org/grok-build), [overview](https://docs.x.ai/build/overview), [settings](https://docs.x.ai/build/settings), [session guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md), [shell reference](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/README.md), and [launch announcement](https://x.ai/news/grok-build-cli).
