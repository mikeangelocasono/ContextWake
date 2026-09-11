# Implementation status

Statuses are evidence-based. `COMPLETE FOR P0` means implemented, integrated, documented, and covered by a passing check; it does not mean every roadmap feature exists.

| Component | Status | Evidence | Remaining work |
|---|---|---|---|
| Terminal-only scope | COMPLETE | No web runtime, frontend, server, or hosted control plane exists | Preserve boundary |
| Rust CLI router | COMPLETE FOR P0 | Coherent CLI/JSON output and completion generation | Final package-name clearance |
| TUI | COMPLETE FOR P0 | Real service state; dashboard, agent capabilities, profiles, workspaces, sessions/detail, checkpoints/detail, handoffs/preview, activity, settings, diagnostics, onboarding, auth help, key help; responsive render tests | Event-driven background refresh and richer model picker |
| Domain model | COMPLETE FOR P0 | Agent, ModelProvider, Model, Profile, Workspace, Session, Checkpoint, Handoff, GitSnapshot, UsageEvent concepts are distinct | Persistence/query APIs for dynamic model catalogs |
| Adapter architecture | COMPLETE FOR P0 | Compile-time `AgentAdapter`, graded capabilities, four adapters, dynamic model/session hooks | Signed/sandboxed external adapter design is future work |
| Codex adapter | PARTIAL | Detection/version/auth/login/logout/model selection/local backends/native resume/handoff commands implemented from official interfaces | Disposable authenticated resume QA; stable session listing unavailable |
| Claude Code adapter | PARTIAL | Detection/version/auth/login/logout/model selection/multiple hosted modes/native resume/handoff commands implemented from official interfaces | Disposable authenticated resume QA; macOS Keychain isolation QA; provider session parser |
| Gemini CLI adapter | PARTIAL | Official `GEMINI_CLI_HOME`, version/model/resume/prompt interfaces implemented; extensions disabled and auth remains truthfully unknown; contract tests pass | Install real CLI for live QA; machine auth status/session list unavailable |
| OpenCode adapter | PARTIAL | Detection/auth presence/login, isolated XDG roots, JSON session sync, native resume command, dynamic models, custom/local backends, and AWHF launch implemented; installed 1.18.25 QA passed | Disposable authenticated resume/handoff QA; cross-platform credential isolation; provider-specific logout UX |
| Kiro CLI adapter | NOT STARTED | Official auth/session/model/`KIRO_HOME` behavior documented; CLI absent locally | Install real CLI, then implement and contract-test |
| SQLite state | COMPLETE FOR P0 | Schema v4, quick check, transactions, v1/v2/v3 migrations with data-preservation tests | Backup/recovery command |
| Workspace manager | COMPLETE FOR P0 | Git/non-Git registry and trusted project metadata | Nested worktree enrichment |
| Git snapshot | COMPLETE FOR P0 | Branch, clean/dirty, staged/unstaged/untracked, ahead/behind, conflicts, numstat, last commit; temp-repository integration tests | Background refresh and large-repository benchmark |
| Profile metadata | COMPLETE FOR P0 | Agent/backend/model-aware CRUD and active profile; no secret columns | Rename command |
| Profile isolation | PARTIAL | Separate `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `GEMINI_CLI_HOME`, and OpenCode XDG/config roots | OS-specific credential/keyring matrix and macOS QA |
| Transactional profile switch | COMPLETE FOR P0 | Shared CLI/TUI service validates target agent, creates artifacts, activates last; failure regression tests | Optional combined launch transaction |
| Native resume | PARTIAL | Same-agent/same-profile/native-record guards and real adapter commands | Authenticated disposable-session manual QA |
| Local session index | COMPLETE FOR P0 | List/show/archive/search/filter, provider title, continuation type, workspace pointer, restart persistence, explicit OpenCode JSON ingestion | Additional provider session ingestion |
| Checkpoints | COMPLETE FOR P0 | v2 JSON, Git, notes, trusted instructions, validation results, redaction, CRUD/export; v1 read compatibility | Editable checkpoint flow and JSON Schema validation in-process |
| AWHF handoffs | COMPLETE FOR P0 | AWHF 1.1 JSON/Markdown, source agent/backend/model, hashes, bounded import/export, tamper/path/symlink/secret/control checks; v1.0 read compatibility | Property fuzzing and optional signed provenance |
| Context Guardian | COMPLETE FOR P0 | Labeled local checkpoint-age/Git-drift indicators; never a fake provider percentage | Background recommendations and non-Git activity signals |
| Provider usage | UNSUPPORTED | No adapter labels local observations as provider quota; OpenCode local stats remain unintegrated | Add only when a reliable supported interface exists |
| Local activity | COMPLETE FOR P0 | SQLite events and separate CLI/TUI section | Duration tracking |
| Diagnostics | COMPLETE FOR P0 | Config, DB, Git, all four implemented agents, auth boundary, terminal; safe verbose paths | Deeper Windows/macOS permission checks |
| Security controls | COMPLETE FOR P0 | Fixed argv, bounded probes and diagnostics, redaction, terminal sanitization, containment, symlink defense, transactional switching | Fuzzing and external security review |
| Open-source readiness | COMPLETE FOR ALPHA | README, Apache-2.0, governance, security policy, changelog, docs, schemas, issue/PR templates, CI matrix | Signed release workflow and naming clearance |

## Latest validation evidence

- Ubuntu WSL, Rust 1.88.0: `cargo fmt --all -- --check` passed.
- Ubuntu WSL: strict Clippy passed with all targets/features and warnings denied.
- Ubuntu WSL: 70 tests passed (59 library, 4 CLI workflow, 2 continuity, 2 Git/workspace, 2 published-schema contracts, 1 session guard); 0 failed.
- Ubuntu WSL: locked optimized release build passed.
- Ubuntu WSL: the Cargo source package verified from a clean extraction (62 files, 555.3 KiB unpacked, 117.9 KiB compressed); explicit inclusion rules exclude the PRD and local build/QA state.
- RustSec `cargo audit`: 251 locked dependencies scanned; exit code 0 with no vulnerability findings.
- Windows, Rust 1.98.1/MSVC after Smart App Control was disabled: clean format and strict Clippy passed; 65 native tests passed (54 library plus 11 integration/contract), 0 failed and 0 ignored. The previously blocked Git integration executable ran successfully.
- Windows locked optimized release build passed. The 6,388,736-byte `adeck.exe` ran `--version`, `status`, and `doctor` from a separate temporary directory, created isolated first-run config/SQLite state, and reported version `0.1.0-alpha.1`.
- The real Windows TUI opened in a PTY, rendered detected agents and the current workspace, navigated Help and Profiles, restored the alternate screen, and exited normally with code 0. No execution-blocking dialog appeared.
- Windows manual QA detected Codex 0.154.0, Claude Code 2.1.267, and OpenCode 1.18.25. OpenCode QA created isolated profiles, reported zero provider credentials without reading them, listed live models, normalized and persisted `opencode/big-pickle`, synchronized an empty native session list, persisted across restarts, and passed doctor. Earlier QA also registered the dirty workspace, created checkpoint v2/AWHF 1.1 artifacts, switched Codex→Claude with handoff continuity, reported no fabricated provider usage, and launched/navigated the real TUI.

## Release blockers

- The milestone is not V1: real authenticated native-resume QA has not been performed with disposable Codex, Claude, and OpenCode identities.
- Signed packages/installers and a trusted update channel do not exist.
- Windows Authenticode signing and macOS physical-device/credential-isolation QA remain external platform work.
- Kiro CLI is not implemented; Gemini CLI needs installed-binary QA.

These limits are also reflected in the [compatibility matrix](docs/providers/compatibility.md); no UI value claims otherwise.
