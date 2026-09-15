# Adding a coding-agent provider

ContextWake adapters are compile-time Rust implementations. Do not add a name to the registry or README until it exposes useful, safely testable terminal functionality.

## 1. Establish official evidence

Record official documentation, official source, current `--version`/`--help`, executable names, authentication ownership, data roots, sessions/resume, models, non-interactive and structured output, ACP/MCP, and platform limits. Keep coding agents separate from model providers.

## 2. Implement `AgentAdapter`

Add a module under `src/provider/` with a stable lowercase ID, display name, aliases, model-provider declarations, and every required trait method. Provider-specific arguments and environment variables stay in the adapter, not the application, store, or TUI.

## 3. Declare graded capabilities

Use `Verified`, `Partial`, `Experimental`, `Unsupported`, or `Unknown` with an evidence-bearing detail. A documented resume command is `Partial` until a real continuity challenge proves the earlier context was restored. Unsupported methods return `CapabilityUnavailable`; they are not simulated.

## 4. Detect safely

Resolve only absolute/system candidates outside the current untrusted workspace. Generic names such as `agent` require vendor-specific version and help signatures. Support an explicit `CONTEXTWAKE_<AGENT>_BIN` override for controlled QA. Never invoke through a shell.

## 5. Use shared process and parser controls

Use `provider::common::run_probe` for timeout, bounded output, child kill/reap, sanitization, and redacted diagnostics. Prefer JSON, JSONL, JSON-RPC, or ACP. Validate all external identifiers, contain provider-reported paths, bound file reads and record counts, and never persist raw secret-bearing output.

## 6. Keep authentication provider-owned

Create only non-secret profile directory structure. Prove configuration and identity separation before reporting profile isolation. Do not copy credentials, infer authentication by spending a paid request, or add plaintext secrets to SQLite.

## 7. Integrate sessions and handoffs

Map only stable public metadata into `DiscoveredAgentSession`; use generic metadata rather than database columns. Native resume must validate agent/profile/session identity. Every adapter needs a portable AWHF start path in both source and destination directions unless a documented blocker makes it partial.

## 8. Add transport only when useful

If the agent exposes stable ACP, return a fixed `AcpTransport` with executable, arguments, and non-secret environment. ContextWake owns process lifecycle. Do not claim ACP merely because the agent can connect to MCP servers.

## 9. Add tests

Cover missing and valid executables, wrong vendor signatures, malformed version/structured output, timeout/failure, ANSI/Unicode, oversized output, unsupported auth/capability behavior, containment, credential non-creation, and shell-free launch arguments. Mock tests are `CONTRACT`, never live `VERIFIED` evidence.

## 10. Register and document

Register the adapter in `AgentRegistry`, update registry/CLI/TUI/doctor contract tests, add `docs/providers/<id>.md`, update the compatibility matrix and candidates, and record privacy/network behavior. Run format, locked check, strict Clippy, all tests, release build, RustSec audit, package verification, and three-platform CI.

An adapter is complete only when its displayed status matches evidence and adding it does not introduce provider-name branches in generic logic.
