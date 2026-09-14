# Cross-agent continuity QA

Reviewed 14 September 2026. Automated contract tests require no coding-agent account and spend no provider tokens. Live comprehension tests are separate manual evidence and are never inferred from a passing mock.

## Deterministic fixture

Project: `contextwake-continuity-fixture`

- Objective: Add CSV export to a CLI application.
- Completed: JSON export; table rendering.
- Current task: Implement CSV writer.
- Decision: Use streaming output.
- Constraint: No new runtime dependency.
- Known issue: Fields containing commas need quoting.
- Pending: Quoting; tests; docs.
- Git state: one staged file, one unstaged file, one untracked file.

`tests/continuity.rs` creates a disposable real Git repository, captures a provider-neutral checkpoint, and verifies this data after every directed AWHF handoff.

## Representative matrix

| Flow | Artifact contract | Destination comprehension |
|---|---|---|
| Codex → GitHub Copilot | PASS | NOT TESTED — Copilot unavailable |
| GitHub Copilot → Claude | PASS | NOT TESTED — Copilot unavailable |
| Claude → Cursor | PASS | NOT TESTED — paid prompt not authorized for this milestone |
| Cursor → OpenCode | PASS | NOT TESTED — paid prompt not authorized |
| OpenCode → Kimi | PASS | NOT TESTED — Kimi unavailable |
| Kimi → Grok Build | PASS | NOT TESTED — Kimi unavailable and Grok isolated profile signed out |
| Grok Build → Codex | PASS | NOT TESTED — Grok isolated profile signed out |

OpenCode's local-provider model classification and handoff path preserve the local-model → commercial-agent and commercial-agent → local-model architecture, but a live Ollama/commercial two-way model run was not performed. Status: `CONTRACT PASS`, `LIVE NOT TESTED`.

## Manual comprehension protocol

For an explicitly authorized disposable profile:

1. Recreate the fixture and start the source agent.
2. Create a ContextWake checkpoint and destination-targeted AWHF handoff.
3. Launch the destination through `ctx handoff continue <id>`.
4. Ask: “Summarize the current objective, current task, key decision, modified files, and next step.”
5. Compare the response with the fixture above and record `PASS`, `PARTIAL`, or `FAIL`, the exact CLI version, profile boundary, platform, and whether provider usage was incurred.

A launch exit code alone is not native-resume or comprehension evidence. No cloud-agent mode may be used unless the tester explicitly selects and records it.
