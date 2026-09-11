# ADR 0003: Coding-agent adapter model

- Status: Accepted
- Date: 2026-09-11

## Decision

Use a compile-time `AgentAdapter` interface with explicit graded capabilities. Keep coding agents, model providers, and models separate.

Adapters own installation/version probes, authentication orchestration, session discovery and launch/resume commands, model-provider declarations, model-reference normalization, dynamic catalogs where supported, and agent-home initialization. Unsupported behavior returns a typed capability error; it is never simulated.

## Rationale

Codex, Claude Code, Gemini CLI, OpenCode, and Kiro expose different authentication, session, model, and machine-readable surfaces. A lowest-common-denominator boolean API would either lie or scatter agent-specific branches through the core. Graded `SUPPORTED`, `PARTIAL`, `UNSUPPORTED`, `UNKNOWN`, and `EXPERIMENTAL` values let the UI explain reality.

## Consequences

The current alpha ships Codex, Claude, Gemini, and OpenCode adapters. OpenCode's custom-provider support is validated against its live model catalog rather than reduced to a static allow-list. Gemini demonstrates a conservative partial adapter: supported resume/model/launch behavior exists while unavailable auth status and machine session listing remain unavailable. Research alone does not register an adapter. Dynamic plugins are deferred because executing arbitrary downloaded code without signing or sandboxing would undermine the threat model.
