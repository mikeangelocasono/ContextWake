# Coding-agent compatibility

Reviewed 14 September 2026. `VERIFIED` means the relevant behavior was exercised against a real CLI or artifact flow. `PARTIAL (contract)` means safe adapter tests passed without credentials. `UNSUPPORTED`, `UNKNOWN`, `NOT TESTED`, and `PENDING AUTH` are intentionally not checkmarks.

| Agent | Detection | Tested version | Auth | Profile isolation | Session listing | Native resume | Models | Structured output | ACP | Handoff | Windows | Linux | macOS | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Codex CLI | VERIFIED | 0.154.0 | VERIFIED | PARTIAL | UNSUPPORTED stable listing | PENDING AUTH | PARTIAL selection | PARTIAL | UNSUPPORTED | VERIFIED artifact | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Claude Code | VERIFIED | 2.1.270 | VERIFIED | PARTIAL | UNSUPPORTED parser | PENDING AUTH | PARTIAL selection | PARTIAL | UNSUPPORTED | VERIFIED artifact | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| GitHub Copilot CLI | PARTIAL (contract) | NOT TESTED | UNKNOWN | PARTIAL | PARTIAL (contract) | PENDING AUTH | PARTIAL selection | PARTIAL | PARTIAL | PARTIAL (contract) | NOT TESTED | NOT TESTED | NOT TESTED | PARTIAL |
| Cursor CLI | VERIFIED | 2026.07.23-e383d2b | VERIFIED | UNSUPPORTED identities / PARTIAL settings | UNSUPPORTED | PENDING AUTH | VERIFIED dynamic | PARTIAL | PARTIAL | PARTIAL (contract) | VERIFIED | NOT TESTED | NOT TESTED | PARTIAL |
| OpenCode | VERIFIED | 1.18.25 | PARTIAL | PARTIAL | VERIFIED JSON | PENDING AUTH | VERIFIED dynamic/local | VERIFIED | PARTIAL | VERIFIED artifact | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Gemini CLI | VERIFIED | 0.59.0 | UNKNOWN | PARTIAL | UNSUPPORTED parser | PENDING AUTH | PARTIAL selection | PARTIAL | EXPERIMENTAL | PARTIAL (contract) | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Kiro CLI | VERIFIED prior QA | 2.21.3 | VERIFIED prior QA | UNSUPPORTED Windows credential | VERIFIED JSON | VERIFIED same identity | VERIFIED dynamic | VERIFIED JSON | UNSUPPORTED | PARTIAL (contract) | VERIFIED prior QA | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Kimi Code CLI | PARTIAL (contract) | NOT TESTED | UNKNOWN | PARTIAL | PARTIAL (contract) | PENDING AUTH | PARTIAL selection | PARTIAL | PARTIAL | PARTIAL | NOT TESTED | NOT TESTED | NOT TESTED | PARTIAL |
| Grok Build | VERIFIED | 0.2.114 | VERIFIED signed-out boundary | PARTIAL | PARTIAL (contract) | PENDING AUTH | VERIFIED dynamic | PARTIAL | PARTIAL | PARTIAL (contract) | VERIFIED | NOT TESTED | NOT TESTED | PARTIAL |

Provider usage is not shown as subscription quota unless an official reliable interface exists. Grok's per-session totals are distinct from subscription balance. macOS and Linux contract cells describe core/adapter CI, not installed-agent or physical-device QA.

The deterministic AWHF contract matrix covers Codex → Copilot → Claude → Cursor → OpenCode → Kimi → Grok → Codex while preserving narrative and dirty Git state. It does not count as destination-model comprehension. Authenticated comprehension results remain `NOT TESTED`.
