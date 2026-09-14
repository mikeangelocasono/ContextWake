# Alpha.2 release provider QA matrix

Reviewed 14 September 2026. This matrix separates live provider evidence from
contract and CI evidence. Each capability cell uses only `VERIFIED`, `PARTIAL`,
`PENDING AUTH`, `UNSUPPORTED`, or `NOT TESTED`. A version number means that exact
CLI was observed on the Windows QA host; Kiro's version is retained prior QA.

| Agent | Installed locally | Version | Detection | Authentication | Profile isolation | Model discovery | Model selection | Session creation | Session listing | Native resume | Structured output | ACP | MCP | Handoff input | Handoff output | Windows | Linux | macOS CI | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Codex CLI | VERIFIED | 0.154.0 | VERIFIED | VERIFIED | PARTIAL | UNSUPPORTED | PARTIAL | NOT TESTED | UNSUPPORTED | NOT TESTED | PARTIAL | UNSUPPORTED | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | Auth status was rechecked; no live inference was spent. |
| Claude Code | VERIFIED | 2.1.270 | VERIFIED | VERIFIED | PARTIAL | UNSUPPORTED | PARTIAL | NOT TESTED | UNSUPPORTED | NOT TESTED | PARTIAL | UNSUPPORTED | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | Live inference was refused at the provider quota boundary before context processing. |
| GitHub Copilot CLI | VERIFIED | 1.0.83 | VERIFIED | VERIFIED | PARTIAL | UNSUPPORTED | PARTIAL | VERIFIED | VERIFIED | VERIFIED | VERIFIED | PARTIAL | PARTIAL | PARTIAL | VERIFIED | VERIFIED | PARTIAL | PARTIAL | Named-session resume recalled 4/4 facts. Codex-to-Copilot comprehension scored 5/7. |
| Cursor CLI | VERIFIED | 2026.09.10-fd3934a | VERIFIED | VERIFIED | UNSUPPORTED | VERIFIED | VERIFIED | VERIFIED | UNSUPPORTED | VERIFIED | PARTIAL | PARTIAL | PARTIAL | VERIFIED | VERIFIED | VERIFIED | PARTIAL | PARTIAL | ContextWake resume recalled 4/4 facts after normal interactive session creation. Copilot-to-Cursor and Cursor-to-OpenCode each scored 7/7. |
| OpenCode | VERIFIED | 1.18.25 | VERIFIED | VERIFIED | PARTIAL | VERIFIED | VERIFIED | VERIFIED | VERIFIED | NOT TESTED | VERIFIED | PARTIAL | PARTIAL | VERIFIED | PARTIAL | VERIFIED | PARTIAL | PARTIAL | OpenRouter-backed planning mode reconstructed the Cursor handoff at 7/7 without writes. |
| Gemini CLI | VERIFIED | 0.59.0 | VERIFIED | NOT TESTED | PARTIAL | UNSUPPORTED | PARTIAL | NOT TESTED | UNSUPPORTED | PENDING AUTH | PARTIAL | PARTIAL | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | Current non-interactive auth status is not exposed reliably. |
| Kiro CLI | NOT TESTED | 2.21.3 | VERIFIED | VERIFIED | UNSUPPORTED | VERIFIED | VERIFIED | VERIFIED | VERIFIED | VERIFIED | VERIFIED | UNSUPPORTED | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | These are retained prior Windows QA results; Kiro was absent during this release pass. |
| Kimi Code CLI | VERIFIED | 0.42.0 | VERIFIED | PENDING AUTH | PARTIAL | NOT TESTED | NOT TESTED | PENDING AUTH | PARTIAL | PENDING AUTH | PARTIAL | PARTIAL | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | Current TypeScript CLI and empty JSON session index were exercised; no provider was configured. |
| Grok Build | VERIFIED | 1.0.30 | VERIFIED | PENDING AUTH | PARTIAL | VERIFIED | VERIFIED | PENDING AUTH | PARTIAL | PENDING AUTH | PARTIAL | PARTIAL | PARTIAL | PARTIAL | PARTIAL | VERIFIED | PARTIAL | PARTIAL | Signed-out boundary and model catalog were verified; authenticated inference was unavailable. |

`PARTIAL` in a platform column means the Rust adapter/contract suite ran in CI,
not that the proprietary agent was installed on that runner. Detection itself
does not send repository contents. No credentials, account identifiers, or
provider tokens are stored in this document.
