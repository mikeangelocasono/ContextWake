# ADR 0005: Short canonical CLI command

- Status: Accepted
- Date: 2026-09-13

## Decision

Keep **ContextWake** as the project identity and `contextwake` as the Rust
package, while making `ctx` the canonical terminal command. Ship a second
`ctxwake` binary during the alpha migration period; both entrypoints delegate
to the same `contextwake::run` implementation.

## Collision audit

This is a practical command collision audit, not trademark or legal clearance.
On the development Windows host and WSL environment, `where.exe ctx`,
PowerShell `Get-Command ctx`, `command -v ctx`, and `which ctx` found no
existing command. Direct registry probes found a crates.io package named `ctx`
that is a Rust library (not a command binary) and an npm package named `ctx`
with no published `bin` entry. The web audit did find unrelated open-source projects that
also use the generic `ctx` command, including [ctxrs/ctx](https://github.com/ctxrs/ctx)
and [vladisov/ctx](https://github.com/vladisov/ctx). This is a real ecosystem
collision risk, so installers and package managers must never overwrite an
existing executable without explicit user control. It is not a shell builtin
or system-level collision on the supported platforms, and the repository uses
an explicit binary name rather than an installer that mutates an existing PATH
entry; under that distinction `ctx` remains the strongest ergonomic choice.
The audit should be repeated by downstream packagers because user PATHs are
not uniform.

| Candidate | Assessment | Decision |
|---|---|---|
| `ctx` | Short, memorable, absent from the audited development PATH; no common shell builtin collision found | **Selected** |
| `cw` | Short but less clearly associated with ContextWake and used by unrelated tools | Fallback only |
| `ctw` | Lower collision risk, but less memorable and less ergonomic | Fallback only |
| `wake` | Natural language command with unrelated software/tool collisions | Rejected |
| `ctwake` | Distinct but longer than needed | Rejected |

## Compatibility and migration

- `ctx` is used in current help, diagnostics, documentation, examples,
  completions, CI smoke tests, and release archives.
- `ctxwake` remains available as a compatibility executable in the next alpha
  archives and reports the same ContextWake version.
- The immutable `v0.1.0-alpha.1` release is not changed; its `ctxwake` binary
  remains the documented historical command.
- The package name stays `contextwake`; state/configuration stays under the
  existing `.contextwake` project directory and `CONTEXTWAKE_HOME` override.
- No database reset or provider behavior change is required.
