# ADR 0004: ContextWake project identity

- Status: Accepted
- Date: 2026-09-11

## Decision

Use **ContextWake** as the provider-neutral project name, `contextwake` as the Rust crate and recommended repository name, `ctxwake` as the executable, and `.contextwake` as the repository-local configuration directory.

The positioning remains: **One workspace. Any coding agent. Keep your context.** A wake is the durable, inspectable project trail that lets work continue behind a previous coding-agent session without claiming that private model context moved.

## Practical collision audit

This is an engineering collision audit, not legal advice or formal trademark clearance. Results are point-in-time observations from 11 September 2026 and must be rechecked before public publication.

| Candidate | Finding | Decision |
|---|---|---|
| AgentDeck | Multiple current AI products and repositories use the exact or close name | Rejected |
| ContextPort | Active AI context products use `contextport.io` and `contextport.com`; an exact GitHub project also exists | Rejected |
| ContextFerry | Existing GitHub projects and Homebrew distribution | Rejected |
| HandoffForge | Existing released software at `Mwoodring2/HandoffForge` | Rejected |
| AgentWaypoint | Exact GitHub repository at `ToDayL/AgentWaypoint` | Rejected |
| ContextWake | No exact GitHub repository-name result, crates.io package, npm package, PyPI project, or Homebrew formula/cask found; no competing software product appeared in exact-name web searches | Accepted |

Additional point-in-time checks found zero exact results for the `ctxwake` crate/package shorthand. RDAP returned not-found for `contextwake.com`, `.io`, and `.dev`; this is informational only and no domain ownership is required by this local-first project.

## Migration and compatibility

- `CONTEXTWAKE_HOME` is the new explicit state-root override. The legacy `AGENTDECK_HOME` remains accepted for existing scripts.
- Default OS application directories are moved from the legacy AgentDeck location only when the corresponding ContextWake directory does not already exist. Existing destinations are never overwritten.
- `.contextwake/project.toml` takes precedence; `.agentdeck/project.toml` remains readable.
- New provider executable overrides use `CONTEXTWAKE_<AGENT>_BIN`; legacy `AGENTDECK_<AGENT>_BIN` variables remain accepted at lower precedence.
- The binary is renamed to `ctxwake`; no permanent legacy binary is shipped during the pre-release period.

## Consequences

The public identity is no longer tied to Codex or any single coding agent. Existing alpha state remains recoverable, while new documentation, schemas, diagnostics, package metadata, and user-visible strings consistently use ContextWake.
