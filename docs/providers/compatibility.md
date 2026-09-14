# Coding-agent compatibility

Reviewed 14 September 2026. `VERIFIED` means the relevant behavior was exercised against a real CLI or artifact flow. `PARTIAL (contract)` means safe adapter tests passed without credentials. `UNSUPPORTED`, `UNKNOWN`, `NOT TESTED`, and `PENDING AUTH` are intentionally not checkmarks.

| Agent | Detection | Tested version | Auth | Profile isolation | Session listing | Native resume | Models | Structured output | ACP | Handoff | Windows | Linux | macOS | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Codex CLI | VERIFIED | 0.154.0 | VERIFIED | PARTIAL | UNSUPPORTED stable listing | PENDING AUTH | PARTIAL selection | PARTIAL | UNSUPPORTED | VERIFIED artifact | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Claude Code | VERIFIED | 2.1.270 | VERIFIED | PARTIAL | UNSUPPORTED parser | PENDING AUTH | PARTIAL selection | PARTIAL | UNSUPPORTED | VERIFIED artifact | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| GitHub Copilot CLI | VERIFIED | 1.0.83 | VERIFIED request | PARTIAL | VERIFIED metadata | VERIFIED known-state | PARTIAL selection | VERIFIED JSONL | PARTIAL | PARTIAL 5/7 input; VERIFIED output | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Cursor CLI | VERIFIED | 2026.09.10-fd3934a | VERIFIED | UNSUPPORTED identities / PARTIAL settings | UNSUPPORTED | VERIFIED known-state | VERIFIED dynamic | PARTIAL | PARTIAL | VERIFIED 7/7 input/output | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| OpenCode | VERIFIED | 1.18.25 | VERIFIED request | PARTIAL | VERIFIED JSON | NOT TESTED | VERIFIED dynamic/local | VERIFIED | PARTIAL | VERIFIED 7/7 input | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Gemini CLI | VERIFIED | 0.59.0 | UNKNOWN | PARTIAL | UNSUPPORTED parser | PENDING AUTH | PARTIAL selection | PARTIAL | EXPERIMENTAL | PARTIAL (contract) | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Kiro CLI | VERIFIED prior QA | 2.21.3 | VERIFIED prior QA | UNSUPPORTED Windows credential | VERIFIED JSON | VERIFIED same identity | VERIFIED dynamic | VERIFIED JSON | UNSUPPORTED | PARTIAL (contract) | VERIFIED prior QA | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Kimi Code CLI | VERIFIED | 0.42.0 | PENDING AUTH | PARTIAL | PARTIAL empty JSON | PENDING AUTH | NOT TESTED | PARTIAL | PARTIAL | PARTIAL (contract) | VERIFIED | PARTIAL (contract) | PARTIAL (contract) | PARTIAL |
| Grok Build | VERIFIED | 1.0.30 | VERIFIED signed-out boundary | PARTIAL | PARTIAL (contract) | PENDING AUTH | VERIFIED dynamic | PARTIAL | PARTIAL | PARTIAL (contract) | VERIFIED | NOT TESTED | NOT TESTED | PARTIAL |

Provider usage is not shown as subscription quota unless an official reliable interface exists. Grok's per-session totals are distinct from subscription balance. macOS and Linux contract cells describe core/adapter CI, not installed-agent or physical-device QA.

The deterministic AWHF contract matrix covers Codex -> Copilot -> Claude -> Cursor -> OpenCode -> Kimi -> Grok -> Codex while preserving narrative and dirty Git state. Live comprehension is separate: Codex -> Copilot scored 5/7 (`PARTIAL`), while Copilot -> Cursor and Cursor -> OpenCode each scored 7/7 (`PASS`). See the [release QA matrix](../qa/release-provider-matrix.md) and [continuity evidence](../qa/cross-agent-continuity.md).
