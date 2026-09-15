# Deterministic continuity demo

`continuity.ps1` exercises real ContextWake workspace, Git, checkpoint, AWHF, transactional-switch, persistence, and status code against a disposable repository. It uses the ContextWake executable as a **version-only provider fixture** so the public demo needs no provider credentials and never presents mock agent output as live AI behavior.

Build the release binary, then run:

```powershell
./demo/continuity.ps1
```

The script leaves the disposable repository and isolated state under the operating-system temporary directory for inspection and prints both paths. It never launches a coding agent, authenticates, executes repository validation commands, commits user repositories, or calls a network service.

The broader automated CSV fixture and seven-hop representative matrix are documented in [cross-agent continuity QA](../docs/qa/cross-agent-continuity.md).

A GIF/asciinema capture is intentionally deferred until the recording tool is available. Any future recording must retain the explicit `DETERMINISTIC PROVIDER FIXTURE` label.
