# Development

## Prerequisites

- Rust 1.88 or newer
- Git
- Codex CLI, Claude Code, Gemini CLI, OpenCode, and Kiro CLI only for optional live adapter smoke tests
- Windows: Visual Studio Build Tools with the C++ workload

## Quality gates

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo build --locked --release
cargo audit
```

Tests use temporary application roots and repositories. CI must never require personal provider credentials.

## Module map

- `app.rs`: application orchestration and human/JSON command output
- `store.rs`: SQLite schema and queries
- `workspace.rs`, `git.rs`: local workspace evidence
- `provider/`: coding-agent contract, registry, and Codex/Claude/Gemini/OpenCode/Kiro adapters
- `checkpoint.rs`, `handoff.rs`, `continuity.rs`: continuity vertical slice
- `security.rs`: terminal sanitization, path rules, redaction
- `tui/`: terminal application and responsive rendering

Integration tests create disposable Git repositories and isolated `CONTEXTWAKE_HOME` roots. Versioned checkpoint and AWHF manifests are published in `schemas/`. Handoff fixtures must contain synthetic values only. Live authentication tests are deliberately excluded from public CI.
