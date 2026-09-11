# Verified agent compatibility

Reviewed 11 September 2026. “Documented” is research evidence, not an implemented adapter. “Local” usage means activity observed by the agent/tool; it is not subscription quota.

| Agent | Detection | Auth status | Session listing | Native resume | Models/backends | Usage | AWHF launch |
|---|---|---|---|---|---|---|---|
| Codex | IMPLEMENTED | IMPLEMENTED | Stable listing unavailable | IMPLEMENTED, same profile | Selection implemented; OpenAI/custom/local documented | Provider quota unavailable | IMPLEMENTED |
| Claude Code | IMPLEMENTED | IMPLEMENTED | Native picker documented; parser deferred | IMPLEMENTED, same profile | Selection implemented; four hosted modes declared | Provider quota unavailable | IMPLEMENTED |
| OpenCode | IMPLEMENTED; 1.18.25 locally tested | PARTIAL credential-presence status | IMPLEMENTED from official JSON | IMPLEMENTED, same profile; live auth QA pending | Dynamic catalog/custom/local backends IMPLEMENTED | Local stats documented; not integrated | IMPLEMENTED |
| Gemini CLI | IMPLEMENTED; live binary QA pending | Status unavailable; interactive flow implemented | Human listing not parsed | IMPLEMENTED, same profile; live QA pending | Selection implemented; catalog unavailable | Provider quota unavailable | IMPLEMENTED by contract test |
| Kiro CLI | NOT IMPLEMENTED; CLI absent locally | JSON command documented | Command documented | Command documented | JSON command documented | Not integrated | NOT IMPLEMENTED |

The provider-specific evidence and limits are recorded in the adjacent documents. All five can use the provider-neutral checkpoint/AWHF file formats; only a registered adapter can start or resume that agent through AgentDeck.
