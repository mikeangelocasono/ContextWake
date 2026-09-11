# Verified agent compatibility

Reviewed 11 September 2026. Allowed values are `VERIFIED`, `PARTIAL`, `UNSUPPORTED`, `UNKNOWN`, and `NOT TESTED`. A documented command is not considered verified until its relevant behavior is exercised. Provider usage means provider-reported quota, never locally observed activity.

| Capability | Codex | Claude Code | OpenCode | Gemini CLI | Kiro CLI |
|---|---|---|---|---|---|
| Detection | VERIFIED 0.154.0 (Windows) | VERIFIED 2.1.267 (Windows) | VERIFIED 1.18.25 (Windows) | VERIFIED 0.59.0 (Windows) | VERIFIED 2.21.3 (Windows) |
| Version | VERIFIED | VERIFIED | VERIFIED | VERIFIED | VERIFIED |
| Auth status | VERIFIED globally; isolated QA signed out | VERIFIED globally; isolated QA signed out | PARTIAL credential-presence check | UNSUPPORTED non-interactively | VERIFIED JSON |
| Isolated profiles | PARTIAL; `CODEX_HOME` | PARTIAL; config directory, keychain QA pending | PARTIAL; XDG roots, credential QA pending | PARTIAL; OAuth/keychain QA pending | UNSUPPORTED on tested Windows credential store |
| Session listing | UNSUPPORTED stable interface | PARTIAL native picker; no parser | VERIFIED JSON | PARTIAL human output; no parser | VERIFIED JSON |
| Native resume | PARTIAL; command/guards verified, authenticated QA pending | PARTIAL; command/guards verified, authenticated QA pending | PARTIAL; command/guards verified, authenticated QA pending | PARTIAL; command/guards verified, authenticated QA pending | VERIFIED same identity |
| Cross-profile native resume | UNSUPPORTED/UNKNOWN; never attempted as bypass | UNSUPPORTED/UNKNOWN | UNSUPPORTED/UNKNOWN | UNSUPPORTED/UNKNOWN | UNSUPPORTED (shared credential) |
| Model detection | PARTIAL selection; no account catalog | PARTIAL declared hosted modes | VERIFIED dynamic catalog | UNSUPPORTED catalog | VERIFIED dynamic catalog |
| Model selection | VERIFIED by contract | VERIFIED by contract | VERIFIED live | VERIFIED by contract | VERIFIED live |
| Context reporting | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED |
| Provider usage reporting | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED | UNSUPPORTED |
| Portable handoff | VERIFIED by contract and artifact flow | VERIFIED by contract and artifact flow | VERIFIED by contract and artifact flow | VERIFIED by contract | VERIFIED by contract |
| Windows QA | VERIFIED core/detection | VERIFIED core/detection | VERIFIED core/discovery | VERIFIED detection/unauthenticated boundary | VERIFIED auth/session/model/resume |
| Linux QA | VERIFIED core/contract tests | VERIFIED core/contract tests | VERIFIED core/contract tests | VERIFIED core/contract tests | VERIFIED core/contract tests |
| macOS CI | NOT TESTED | NOT TESTED | NOT TESTED | NOT TESTED | NOT TESTED |

The provider documents adjacent to this matrix contain commands, official sources, and the precise limitations. Cross-agent Codex to Claude, Claude to OpenCode, and OpenCode to Codex portable artifacts were exercised against a controlled dirty repository; target-model comprehension remains pending disposable authenticated profiles.
