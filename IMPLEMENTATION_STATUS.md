# Implementation status

Statuses are evidence-based: `COMPLETE`, `PARTIAL`, `BLOCKED`, `UNSUPPORTED`, or `NOT STARTED`. `COMPLETE` refers to the current alpha scope, not the entire roadmap.

| Component | Status | Evidence | Remaining work |
|---|---|---|---|
| Terminal-only scope | COMPLETE | No web runtime, frontend, server, or hosted control plane exists | Preserve boundary |
| Rust CLI router | COMPLETE | Coherent CLI/JSON output and completion generation under canonical `ctx`; compatibility `ctxwake` delegates to the same startup path | Shell-completion distribution |
| TUI | COMPLETE | Scrollable nine-agent discovery/details with installation/auth/capability states and `F` filters for installed, authenticated, native resume, and ACP; responsive render tests | Event-driven background refresh and richer model picker |
| Domain model | COMPLETE | Agent, ModelProvider, Model, Profile, Workspace, Session, Checkpoint, Handoff, GitSnapshot, and UsageEvent are distinct | Persistence APIs for richer model metadata |
| Adapter architecture | COMPLETE | Compile-time trait-object registry, aliases, graded capabilities, bounded three-at-a-time detection, shared safe probes/metadata reads, optional ACP transport, and nine adapters | Signed/sandboxed external adapter design is future work |
| Codex adapter | PARTIAL | Detection/version/auth/login/logout/model selection/local backends/native resume/handoff construction use official interfaces | Disposable authenticated resume QA; stable session listing unavailable |
| Claude Code adapter | PARTIAL | Detection/version/auth/login/logout/model selection/hosted modes/native resume/handoff construction use official interfaces | Disposable authenticated resume QA; macOS Keychain isolation QA; provider session parser |
| Gemini CLI adapter | PARTIAL | Gemini CLI 0.59.0 installed/detected on Windows; current npm entry point, `GEMINI_CLI_HOME`, model/resume/prompt interfaces, and unauthenticated boundary verified | Authenticated resume/handoff QA; machine auth status/session list unavailable |
| OpenCode adapter | PARTIAL | Detection/auth presence/login, isolated XDG roots, JSON session sync, native resume construction, dynamic models, custom/local backends, and AWHF launch; installed 1.18.25 QA passed | Disposable authenticated resume/handoff QA and cross-platform credential isolation |
| Kiro CLI adapter | PARTIAL | Vendor-signed 2.21.3 installed; detection, JSON auth/session/model discovery, sanitized persistence, dynamic models, and real same-identity native resume verified | Cross-platform QA and authenticated cross-agent handoff comprehension; isolated credentials unsupported on tested Windows host |
| GitHub Copilot CLI adapter | PARTIAL | Official-current `copilot` interface, signature-gated detection, `COPILOT_HOME`, bounded session metadata, fixed-argv resume/handoff, JSONL/ACP capabilities, parser and failure contract tests | CLI not installed locally; auth, multi-identity isolation, and real resume/comprehension QA pending |
| Cursor CLI adapter | PARTIAL | Real Windows 2026.07.23 detection/auth/model QA; rejects unrelated `agent.exe`; fixed-argv bundle launch, ACP, resume and handoff contracts | Interactive history cannot be safely parsed; authenticated resume pending; tested auth is global despite scoped settings |
| Kimi Code adapter | PARTIAL | Official-current Kimi Code contract, `KIMI_CODE_HOME`, contained/bounded session metadata, ACP and resume/handoff construction with secret-bearing provider catalog excluded | CLI not installed locally; auth/isolation/resume/comprehension QA pending; interactive handoff requires user instruction |
| Grok Build adapter | PARTIAL | Real signed Windows 0.2.114 detection/help/model/signed-out QA; `GROK_HOME`, contained summary metadata, ACP, models, resume and handoff contracts | Authenticated identity isolation, resume, and comprehension QA pending |
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
| Diagnostics | COMPLETE | Config, DB, Git, all nine adapters, explicit optional `Not installed`, auth boundaries, terminal, and safe verbose paths | Deeper Windows/macOS permission checks |
| Security controls | COMPLETE | Fixed argv, bounded probes, redaction, terminal sanitization, containment, symlink defense, and transactional switching | Fuzzing and external security review |
| Open-source readiness | COMPLETE | Public ContextWake repository and `v0.1.0-alpha.1` prerelease, compatibility migration, README and real TUI image, Apache-2.0, governance, security policy, changelog, docs, schemas, templates, green three-platform CI, and verified release artifacts | Optional signing/notarization and future package registries |

## Latest validation evidence

- CLI ergonomics migration: package version `0.1.0-alpha.2` keeps the
  `contextwake` crate and `.contextwake`/`CONTEXTWAKE_HOME` state paths, adds
  canonical `ctx` and compatibility `ctxwake` binaries, and covers both
  entrypoints with CLI regression tests.

- Expanded-agent milestone, Windows Rust 1.98.1/MSVC: format, locked all-target check, strict Clippy, and 110 tests passed before release validation. Bounded nine-agent detection completed in about 2.4 seconds warm on the QA host (6.1 seconds on the first post-build cold run). Codex 0.154.0, Claude Code 2.1.270, Cursor 2026.07.23-e383d2b, OpenCode 1.18.25, Gemini 0.59.0, and Grok Build 0.2.114 were detected; Copilot, Kiro, and Kimi were absent in this run. No paid provider prompt was used.
- Ubuntu WSL, minimum Rust 1.88.0: format, locked all-target check, strict Clippy, 114 tests, and optimized build passed. RustSec scanned all 249 locked dependencies against 1,244 advisories with no vulnerability failure. Clean-tree Cargo packaging passed with 93 files (1.1 MiB unpacked, 517.0 KiB compressed), and the extracted crate compiled.
- The deterministic CSV fixture contract covers Codex → GitHub Copilot → Claude → Cursor → OpenCode → Kimi → Grok → Codex. Every hop retained objective, active task, completed work, streaming-output decision, no-new-dependency constraint, comma-quoting issue, pending tasks, and staged/unstaged/untracked counts. This is artifact-level contract evidence; destination-agent comprehension remains `NOT TESTED`.

- Ubuntu WSL, Rust 1.88.0: final format, strict Clippy, and 89 tests passed with no failures or ignored tests; the locked optimized build and RustSec audit of 249 dependencies passed. Alpha.2 Cargo packaging passed with 80 files (1018.1 KiB unpacked, 488.9 KiB compressed), including the canonical and compatibility entrypoints plus public documentation while excluding ignored local/release material.
- Windows, Rust 1.98.1/MSVC after Smart App Control was disabled: final format, strict Clippy, and 85 native tests passed with no failures or ignored tests, including the formerly blocked Git integration executable; the locked optimized build passed. Alpha.2 `ctx.exe` and `ctxwake.exe` both passed version/help smoke tests.
- The final 6,482,432-byte Windows release executable ran version and status after archive extraction with isolated first-run state; warm version startup was 42.83 ms. The real TUI rendered, navigated Help and Profiles, restored the terminal, exited normally, and left no `ctxwake` process or execution-blocking dialog.
- The final 6,843,512-byte Linux executable retained mode `0755` after archive extraction and ran version/status from a non-repository directory. The Windows and Linux archives contain only the binary, README, and LICENSE; `SHA256SUMS` was generated and verified locally.
- Windows detected Codex 0.154.0, Claude Code 2.1.267, OpenCode 1.18.25, Gemini CLI 0.59.0, and Kiro CLI 2.21.3. Gemini's isolated unauthenticated invocation failed closed. Kiro live QA listed nine models, synchronized and resumed a real session, and exposed its shared Windows credential boundary honestly.
- A controlled dirty repository exercised Codex to Claude, Claude to OpenCode, and OpenCode to Codex AWHF 1.2 artifact flows. Every hop retained objective/task/completed/decisions/pending/issues, branch and HEAD, staged/unstaged/untracked counts, diff summary, validation results, project instructions, source/destination, timestamps, and `portable_handoff`; secret and absolute-path checks passed.
- Public CI run `34576605189` passed RustSec plus format, strict Clippy, tests, and release builds on Ubuntu, Windows, and macOS. The prior macOS job passed 87 tests; this is automated CI evidence, not physical-device TUI QA. The alpha.2 migration adds two CLI compatibility tests for a current total of 89 Linux/macOS tests and 85 Windows tests.
- Manual release-workflow run `34576636745` built and packaged Windows x86_64, Linux x86_64, and macOS arm64 using the current official Node 24 artifact actions. All three archives contain only the native binary, README, and LICENSE; downloaded checksums verified. Downloaded Windows and Linux binaries ran from isolated non-repository directories, and the macOS payload was verified as a mode-`0755` Mach-O arm64 executable.
- Tagged CI run `34577840714` passed all Ubuntu, Windows, macOS, and RustSec jobs on commit `b67143d`. Tagged release run `34577840582` validated the Cargo version, rebuilt all three targets, generated checksums, and published the verified GitHub prerelease at `v0.1.0-alpha.1`.
- Cleanup commit `46fb23c` removed superseded planning/design documents and the unused `predicates` direct dependency, consolidated the retained TUI contract into architecture documentation, and expanded local-artifact ignore rules. CI run `34583329621` passed all Ubuntu, Windows, macOS, and RustSec jobs on the cleaned tree.

## Known alpha limitations

- Disposable authenticated native-resume QA has not been performed for Copilot, Cursor, Kimi, Grok, Codex, Claude, OpenCode, or Gemini; cross-agent target-model comprehension requires deliberately authorized disposable profiles; Windows Authenticode and Apple signing/notarization identities are unavailable; macOS has automated CI and artifact evidence but no interactive physical-device QA.

These limits are reflected in the [compatibility matrix](docs/providers/compatibility.md); no UI value claims otherwise.
