# Implementation status

Statuses are evidence-based: `COMPLETE`, `PARTIAL`, `BLOCKED`, `UNSUPPORTED`, or `NOT STARTED`. `COMPLETE` refers to the current alpha scope, not the entire roadmap.

| Component | Status | Evidence | Remaining work |
|---|---|---|---|
| Terminal-only scope | COMPLETE | No web runtime, frontend, server, or hosted control plane exists | Preserve boundary |
| Rust CLI router | COMPLETE | Coherent CLI/JSON output and completion generation under `ctxwake` | Shell-completion distribution |
| TUI | COMPLETE | Real service state across dashboard, capabilities, profiles, workspaces, sessions, checkpoints, handoffs, activity, settings, diagnostics, onboarding, auth help, and keyboard help; responsive render tests | Event-driven background refresh and richer model picker |
| Domain model | COMPLETE | Agent, ModelProvider, Model, Profile, Workspace, Session, Checkpoint, Handoff, GitSnapshot, and UsageEvent are distinct | Persistence APIs for richer model metadata |
| Adapter architecture | COMPLETE | Compile-time `AgentAdapter`, graded capabilities, five adapters, and dynamic model/session hooks | Signed/sandboxed external adapter design is future work |
| Codex adapter | PARTIAL | Detection/version/auth/login/logout/model selection/local backends/native resume/handoff construction use official interfaces | Disposable authenticated resume QA; stable session listing unavailable |
| Claude Code adapter | PARTIAL | Detection/version/auth/login/logout/model selection/hosted modes/native resume/handoff construction use official interfaces | Disposable authenticated resume QA; macOS Keychain isolation QA; provider session parser |
| Gemini CLI adapter | PARTIAL | Gemini CLI 0.59.0 installed/detected on Windows; current npm entry point, `GEMINI_CLI_HOME`, model/resume/prompt interfaces, and unauthenticated boundary verified | Authenticated resume/handoff QA; machine auth status/session list unavailable |
| OpenCode adapter | PARTIAL | Detection/auth presence/login, isolated XDG roots, JSON session sync, native resume construction, dynamic models, custom/local backends, and AWHF launch; installed 1.18.25 QA passed | Disposable authenticated resume/handoff QA and cross-platform credential isolation |
| Kiro CLI adapter | PARTIAL | Vendor-signed 2.21.3 installed; detection, JSON auth/session/model discovery, sanitized persistence, dynamic models, and real same-identity native resume verified | Cross-platform QA and authenticated cross-agent handoff comprehension; isolated credentials unsupported on tested Windows host |
| SQLite state | COMPLETE | Schema v4, quick check, transactions, and v1/v2/v3 migrations with data-preservation tests | Backup/recovery command |
| Workspace manager | COMPLETE | Git/non-Git registry and trusted project metadata | Nested worktree enrichment |
| Git snapshot | COMPLETE | Branch, clean/dirty, staged/unstaged/untracked, ahead/behind, conflicts, numstat, and last commit; temporary-repository integration tests | Background refresh and large-repository benchmark |
| Profile metadata | COMPLETE | Agent/backend/model-aware CRUD and active profile; no secret columns | Rename command |
| Profile isolation | PARTIAL | Dedicated homes/config roots where supported | macOS credential matrix; Kiro's Windows credential is known shared and never presented as isolated |
| Transactional profile switch | COMPLETE | Shared CLI/TUI service validates target, creates artifacts, and activates last; rollback regression tests | Optional combined launch transaction |
| Native resume | PARTIAL | Same-agent/same-profile/native-record guards and real adapter commands; Kiro same-identity resume verified | Disposable authenticated QA for remaining agents |
| Local session index | COMPLETE | List/show/archive/search/filter, provider title, continuation type, workspace pointer, restart persistence, and JSON ingestion for OpenCode/Kiro | Additional provider ingestion where stable JSON exists |
| Checkpoints | COMPLETE | Versioned JSON, Git, notes, trusted instructions, validation results, redaction, CRUD/export, and legacy reading | Editable checkpoint flow |
| AWHF handoffs | COMPLETE | AWHF 1.2 JSON/Markdown, explicit destination and portable-continuity mode, hashes, bounded import/export, tamper/path/symlink/secret/control checks, and v1.0/v1.1 reads | Property fuzzing and optional signed provenance |
| Context Guardian | COMPLETE | Labeled local checkpoint-age/Git-drift indicators; never a fake provider percentage | Background recommendations and non-Git signals |
| Provider usage | UNSUPPORTED | No adapter labels local observations as provider quota | Add only when an official reliable interface exists |
| Local activity | COMPLETE | SQLite events and separate CLI/TUI section | Duration tracking |
| Diagnostics | COMPLETE | Config, DB, Git, all five adapters, auth boundaries, terminal, and safe verbose paths | Deeper Windows/macOS permission checks |
| Security controls | COMPLETE | Fixed argv, bounded probes, redaction, terminal sanitization, containment, symlink defense, and transactional switching | Fuzzing and external security review |
| Open-source readiness | PARTIAL | ContextWake rename with compatibility migration, README, Apache-2.0, governance, security policy, changelog, docs, schemas, templates, CI matrix, and tag-gated artifact workflow | Hosted CI run and signing/notarization |

## Latest validation evidence

- Ubuntu WSL, Rust 1.88.0: final format and strict Clippy passed; 87 tests passed with no failures or ignored tests; the locked optimized build and RustSec audit of 251 dependencies passed. Final clean Cargo packaging follows the release commit.
- Windows, Rust 1.98.1/MSVC after Smart App Control was disabled: final format and strict Clippy passed; 83 native tests passed with no failures or ignored tests, including the formerly blocked Git integration executable; the locked optimized build passed.
- The renamed 6,487,040-byte Windows release executable ran version, status, and doctor from a separate directory with isolated first-run state; warm version startup was 23.75 ms. The real TUI rendered, navigated Help and Profiles, restored the terminal, exited normally, and left no `ctxwake` process or execution-blocking dialog.
- Windows detected Codex 0.154.0, Claude Code 2.1.267, OpenCode 1.18.25, Gemini CLI 0.59.0, and Kiro CLI 2.21.3. Gemini's isolated unauthenticated invocation failed closed. Kiro live QA listed nine models, synchronized and resumed a real session, and exposed its shared Windows credential boundary honestly.
- A controlled dirty repository exercised Codex to Claude, Claude to OpenCode, and OpenCode to Codex AWHF 1.2 artifact flows. Every hop retained objective/task/completed/decisions/pending/issues, branch and HEAD, staged/unstaged/untracked counts, diff summary, validation results, project instructions, source/destination, timestamps, and `portable_handoff`; secret and absolute-path checks passed.

## Release blockers

- Disposable authenticated native-resume QA has not been performed for Codex, Claude, OpenCode, or Gemini.
- Cross-agent target-model comprehension QA requires deliberately authorized disposable profiles; artifact preservation is verified.
- Windows Authenticode signing and Apple signing/notarization identities are unavailable.
- macOS CI is configured but cannot be claimed until a GitHub-hosted run completes.
- Final clean Cargo packaging, native archive/checksum generation, and artifact validation remain in progress.

These limits are reflected in the [compatibility matrix](docs/providers/compatibility.md); no UI value claims otherwise.
