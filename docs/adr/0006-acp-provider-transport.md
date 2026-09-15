# ADR 0006: Optional ACP provider transport

- Status: Accepted
- Date: 2026-09-14

## Context

OpenCode, GitHub Copilot CLI, Cursor CLI, Kimi Code, and Grok Build expose Agent Client Protocol servers. Structured lifecycle and session communication can be safer and less brittle than parsing human terminal output, but existing adapters do not all support ACP and ContextWake should not be rewritten around it.

## Decision

`AgentAdapter` may return an `AcpTransport`: a fixed executable path, fixed argument vector, and non-secret environment. It builds `std::process::Command` directly and never passes through a shell. The native CLI remains the default launch/resume mechanism; ACP is capability-gated and optional.

ContextWake will add an ACP client/lifecycle manager only for workflows where it materially improves structured session control. The current milestone establishes the truthful transport contract without silently starting an agent, uploading a workspace, or pretending that MCP support is ACP support.

## Consequences

- Providers without ACP remain fully usable through their native CLI adapter.
- Provider homes and authentication remain provider-owned.
- Remote/cloud modes require separate explicit capabilities; Copilot ACP disables remote export and Cursor cloud workers are not launched.
- Future ACP process management must enforce bounded messages, cancellation, timeouts, child kill/reap, JSON-RPC validation, and secret-safe diagnostics.
- The compile-time registry remains the plugin security boundary.
