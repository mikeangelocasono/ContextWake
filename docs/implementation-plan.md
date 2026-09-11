# MVP Implementation Plan

The plan is ordered by working vertical slices. Status and evidence live in [IMPLEMENTATION_STATUS.md](../IMPLEMENTATION_STATUS.md).

1. Establish the Rust binary, strict config, platform data paths, SQLite migrations, JSON errors, and CI.
2. Detect Git/non-Git workspaces and capture bounded, non-blocking repository evidence through fixed argv.
3. Create local profile metadata and dedicated agent process homes; orchestrate supported auth without owning secrets.
4. Index local sessions and permit native resume only under a compatible recorded profile.
5. Generate deterministic checkpoints and integrity-checked AWHF handoffs.
6. Make profile activation transactional around continuity: build the handoff first, activate last.
7. Connect live state to the responsive TUI and cover first-run, empty, error, narrow, and confirmation states.
8. Complete doctor, security tests, cross-platform CI, packaging, and release documentation.
9. Prove provider neutrality with Codex, Claude Code, Gemini CLI, and OpenCode adapters plus normalized Agent/ModelProvider/Model persistence.
10. Keep OpenCode, Gemini CLI, and Kiro capability-driven. OpenCode JSON integration, Gemini 0.59.0 installed detection, and Kiro 2.21.3 JSON session/model plus native-resume QA are complete for their documented alpha boundaries.
11. Research experimental machine-readable session/usage APIs behind version gates after stable fallback paths pass.
12. Harden handoff import with property fuzzing beyond the implemented strict schema, bounds, secret scan, symlink rejection, and integrity checks.
