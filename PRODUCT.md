# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Stack

Delegated: Rust 2024 with clap, ratatui/crossterm, SQLite, serde, and provider subprocess adapters. The TUI adapts to Windows, macOS, Linux, WSL, headless terminals, Unicode capability, and available terminal size.

## Users

The primary users are developers doing AI-assisted work across repositories and legitimate personal, employer, client, organization, or open-source identities. They need to know which identity and workspace are active, resume supported sessions safely, and preserve explicit project state across session or identity boundaries.

## Product Purpose

AgentDeck is a local-first, terminal-native identity, workspace, session, and context continuity manager for AI coding CLIs. Codex and Claude Code are the first implemented coding-agent adapters. Success means the flow “open workspace -> verify agent/profile and Git state -> resume when safe -> checkpoint -> handoff” is dependable from one terminal.

## Positioning

The differentiated mechanism is workspace-centric continuity: provider-owned authentication and native resume are combined with local Git-aware checkpoints and portable, human-reviewable handoffs. The product never claims to transfer hidden reasoning or cross-account conversation ownership.

## Operating Context

Users launch `adeck` in a terminal, inspect live local and supported provider state, choose an authorized profile explicitly, and continue work through the native provider TUI. The product supports Git and non-Git directories, offline checkpoint browsing, and plain or JSON CLI output.

## Capabilities and Constraints

- Terminal-only product; no web dashboard, hosted control plane, or browser management UI.
- Profiles are local metadata around separate provider process/configuration boundaries, never password or token containers.
- Profile switching is explicit and never triggered by quotas, rate limits, or plan state.
- Native resume is used only when the recorded coding agent and profile are compatible.
- Cross-profile, cross-agent, or unsupported continuity uses an explicit checkpoint and handoff.
- Provider usage, model, account, and context values appear only when a reliable supported source supplies them.
- Repository configuration is untrusted, stores commands as argv arrays, and never auto-executes.
- Telemetry is off by default; local activity stays local.
- AgentDeck and the `adeck` package/binary names remain provisional pending release clearance. `cx` is not used because current CLI collisions were verified.

## Brand Commitments

The interface is compact, keyboard-first, terminal-native, restrained, and operationally precise. Lazygit and GitHub CLI establish the expected usability and craft level, but the continuity decision is AgentDeck’s own organizing idea.

## Evidence on Hand

- `AI_CLI_Workspace_Manager_PRD_v1.0.pdf` is the primary specification.
- Official Codex, Claude Code, Gemini CLI, OpenCode, and Kiro CLI documentation was reviewed on 10 September 2026. Installed Codex 0.154.0, Claude Code 2.1.267, and OpenCode 1.18.25 behavior was inspected where safe.
- No customer claims, adoption metrics, screenshots, trademark clearance, or production benchmarks are available and none may be fabricated.

## Product Principles

- Workspace and continuity first.
- Explicit identity, provider truth, and precise terminology.
- Provider-owned secrets with isolated execution boundaries.
- Local-first and inspectable by default.
- Graceful degradation instead of guessed capabilities.

## Accessibility & Inclusion

Every status has a textual marker, not color alone. Core workflows are keyboard-first and have plain/JSON equivalents. The TUI supports narrow terminals and an ASCII-safe mode.
