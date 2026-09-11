# Contributing

Thank you for helping build a safer terminal workflow for AI-assisted development.

## Setup

Install Rust 1.88+, Git, and platform native build tools. Then run:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo build --locked --release
```

Coding-agent credentials are never required in CI. Adapter behavior should use fixtures or isolated temporary agent homes. A configuration home must not be described as credential isolation until that boundary is verified on the target platform.

## Pull requests

- Keep one coherent change per PR.
- Add tests for behavior and failure paths.
- Update docs and `IMPLEMENTATION_STATUS.md` when capability status changes.
- Do not describe experimental or observed behavior as a guaranteed provider contract.
- Never add auto-account rotation, quota-circumvention logic, secret copying, auto-executed repository commands, or web-platform scope.
- Include TUI snapshots/screenshots for visual changes and test 80x24 plus a narrow layout.

Changes touching credentials, process execution, repository trust, handoff import/export, updates, or provider protocol parsing require a security-impact section and maintainer security review.

## Design changes

The TUI is an operator console. Preserve compact hierarchy, textual status markers, keyboard access, honest unavailable states, and responsive collapse. Do not add fake provider values or controls without working behavior.

## RFCs

Use an RFC discussion before adding a provider, changing the AWHF schema, introducing plugins/cloud sync, replacing storage, or breaking the CLI.
