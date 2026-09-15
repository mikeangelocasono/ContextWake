# Additional coding-agent candidates

Reviewed 14 September 2026 using official documentation and source repositories. None of these names is registered as a ContextWake adapter in this milestone; research alone is not support.

| Candidate | Classification | Reason |
|---|---|---|
| [Cline CLI](https://docs.cline.bot/cli/cli-reference) | GOOD FUTURE CANDIDATE | Real terminal agent with `cline`, JSON output, isolated data roots, history/resume, model/provider selection, MCP, ACP, and clear official documentation. Windows live support needs rechecking because current install guidance still notes a platform limitation. |
| [Continue CLI](https://docs.continue.dev/cli/quickstart) | GOOD FUTURE CANDIDATE | `cn` is a documented terminal coding agent with interactive/headless modes, resume, model/config selection, MCP, and cross-platform installation. Authentication/profile behavior needs isolation QA. |
| [goose](https://block.github.io/goose/index.html) | GOOD FUTURE CANDIDATE | Actively maintained native CLI with sessions, multiple model providers, deep MCP support, ACP server/client roles, recipes, and Windows/Linux/macOS support. Its broader general-agent scope needs a precise coding-workspace adapter boundary. |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/) | GOOD FUTURE CANDIDATE | Official `qwen` terminal coding agent with headless composition, MCP, multiple hosted/custom providers, local-compatible endpoints, and current cross-platform docs. It materially expands open/local workflows. |
| [Aider](https://aider.chat/docs/) | GOOD FUTURE CANDIDATE | Mature Git-aware terminal pair programmer with broad hosted/local model support and scripting. Session/native-resume semantics and credential isolation need closer evaluation before registration. |
| [Crush](https://github.com/charmbracelet/crush) | GOOD FUTURE CANDIDATE | Maintained terminal agent with project sessions, multiple providers, MCP, explicit config/data overrides, and broad native platform support. Machine-readable session/control surfaces need verification. |
| [Amp](https://ampcode.com/docs/cli) | GOOD FUTURE CANDIDATE | Serious terminal coding agent with local interactive/execute modes and saved threads. Its optional cloud orbs must remain an explicit separate capability; native Windows is not currently supported outside WSL. |
| Legacy `gh copilot` extension | NOT SUITABLE | Superseded interface and duplicate of the current first-class GitHub Copilot CLI adapter. |
| Legacy `MoonshotAI/kimi-cli` | ABANDONED/UNCERTAIN | The current supported product is Kimi Code; an adapter for the older CLI would create misleading duplicate support. |
| Ollama, OpenAI API, Anthropic API, Grok API | DUPLICATE MODEL BACKEND | These are model runtimes/providers, not terminal coding agents. They belong behind capable agents such as OpenCode, Qwen Code, Aider, or Crush. |

No candidate is `IMPLEMENT NOW`: the four required adapters plus the generic transport/contract work are the reliability boundary for this alpha. Cline is the strongest next-adapter candidate because its current CLI exposes the most complete structured contract.
