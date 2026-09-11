# ADR 0001: Rust and Ratatui stack

- Status: Accepted
- Date: 2026-09-10

## Decision

Use Rust 2024 (MSRV 1.88), Clap, Ratatui, Crossterm, Serde, Rusqlite, and typed errors. Ship one native `ctxwake` binary.

## Rationale

The product needs fast startup, bounded resource use, fixed-argv subprocess control, strict domain types, cross-platform terminal handling, and straightforward binary distribution. Rust provides a strong security and correctness boundary for profile switching, artifact import, path handling, and terminal rendering. Ratatui/Crossterm provide testable buffer rendering across terminal sizes.

Go was viable and contributor-friendly, but changing the existing passing Rust foundation would add risk without a product benefit. Node/Ink would add a runtime and make native process distribution less predictable.

## Consequences

Compilation is heavier than Go, and Windows release builds need C++ build tools because SQLite is bundled. CI covers Windows, macOS, and Linux. Unsafe Rust is forbidden.
