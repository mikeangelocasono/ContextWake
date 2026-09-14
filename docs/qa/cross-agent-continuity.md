# Cross-agent continuity QA

Reviewed 14 September 2026. Automated contract tests require no coding-agent
account and spend no provider tokens. Live comprehension evidence is reported
separately and never inferred from mocks, serialization, or a successful launch.

## Deterministic fixture

Project: `wake-csv-demo`

- Objective: Add CSV export to a Rust CLI.
- Completed: JSON export; table output.
- Current task: Implement CSV export.
- Decision: Use a streaming writer.
- Constraint: No new runtime dependencies.
- Known issue: CSV fields containing commas require quoting.
- Pending: Quoting logic; tests; README example.
- Git state: branch `main`, one staged file, one unstaged file, one untracked
  file, and a fixed baseline HEAD.

The committed test in `tests/continuity.rs` independently recreates an equivalent
disposable repository and verifies the provider-neutral artifact at every hop.
The manual fixture lived under ignored build output and is not part of Git.

## Reproducible comprehension score

One point is awarded for each correct field: objective, current task, completed
work, architectural decision, constraint, known issue, and pending work.

- 7/7: `PASS`
- 5-6/7: `PARTIAL`
- Below 5/7: `FAIL`

Git branch, exact HEAD, staged count, unstaged count, and untracked count are
verified independently from the seven-point semantic score. Every live run used
read-only/plan mode where the destination supported it, and the Git state was
compared before and after.

## Representative matrix

| Flow | Artifact contract | Live destination comprehension | Git fidelity | Result |
|---|---|---|---|---|
| Codex -> GitHub Copilot | PASS | 5/7; objective, task, completed work, known issue, and pending work were correct; decision and constraint were not exact | Unchanged; destination misreported two counts | PARTIAL |
| GitHub Copilot -> Claude Code | PASS | Provider accepted authentication but rejected inference at the weekly quota boundary before reading context | Unchanged | BLOCKED |
| Claude Code -> Cursor | PASS | Not run after the Claude quota boundary | Contract only | NOT TESTED |
| Cursor -> OpenCode | PASS | 7/7 using authenticated OpenCode planning mode | Exact branch, HEAD, and 1/1/1 counts; unchanged | PASS |
| OpenCode -> Kimi Code | PASS | Kimi has no authenticated provider configured | Contract only | BLOCKED |
| Kimi Code -> Grok Build | PASS | Both authenticated source work and Grok authentication are unavailable | Contract only | BLOCKED |
| Grok Build -> Codex | PASS | Grok source session unavailable while signed out | Contract only | BLOCKED |

An additional authenticated GitHub Copilot -> Cursor flow scored 7/7. Cursor
also recovered the exact branch, fixed HEAD, and 1/1/1 Git counts, and did not
modify the fixture. This is the strongest live cross-agent evidence in this
release pass.

## Native-resume evidence

GitHub Copilot CLI 1.0.83 created a named session in an isolated
`COPILOT_HOME`. ContextWake synchronized the provider session, resumed it by
name, and the resumed agent correctly recalled all four challenge facts:
objective, current task, no-new-runtime-dependency constraint, and comma-quoting
issue. This is `VERIFIED` native resume, not a portable handoff.

Cursor 2026.09.10 also passed native resume after normal interactive session
creation. The first ContextWake attempt exposed Cursor's inability to parse a
Windows verbatim `\\?\` workspace argument; after ContextWake normalized only
the provider-bound path, the resumed session recalled all 4/4 challenge facts.
Internal workspace paths remain canonical. Cursor's provider-owned identity was
available across `CURSOR_CONFIG_DIR` roots, so multiple-identity isolation is
`UNSUPPORTED`, not implied by the successful resume. Kimi Code and Grok Build
native resume remain `PENDING AUTH`.

## Local/commercial boundary

OpenCode with an authenticated commercial OpenRouter backend participated in a
live handoff. A local inference runtime such as Ollama was not installed, so
local-model -> commercial-agent and commercial-agent -> local-model live runs
remain `NOT TESTED`; only their adapter and AWHF contracts pass.

## Failure and rollback evidence

- A malformed imported handoff was rejected before persistence.
- A missing optional provider binary remained a nonfatal detection result.
- A failed destination/model entitlement exited before inference.
- A failed destination tool request and a provider quota rejection made no
  repository changes.
- The original handoff bytes, active profile, checkpoint state, and dirty Git
  fixture remained unchanged after the explicit rollback probes.
- Path-like native session references are now rejected before any provider
  process starts and have a regression test.

No cloud-agent mode was used. No account identifier, credential, provider
session transcript, or hidden reasoning is retained in this QA document.
