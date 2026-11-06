# Agent Workspace Handoff Format (AWHF) 1.2.0

The alpha writes a directory package:

```text
handoff/
  manifest.json
  context.md
  git-summary.json
  validation.json
```

`manifest.json` contains the schema version, UUID, UTC creation time, source tool, structured source agent/model-provider/model/profile/session metadata, optional destination agent/profile metadata, the explicit `portable_handoff` continuity mode, checkpoint reference, a workspace path fingerprint, branch/commit, objective, content hashes, redaction counts, and security flags. Agent, model provider, and model are deliberately separate concepts. The source is always present: a checkpoint captured while no profile was active records `"agent_id": "unknown"` with null model provider, model, and profile instead of omitting the metadata that AWHF 1.1 and newer require. The fingerprint is the SHA-256 of the workspace's conventional path (on Windows, without the `\\?\` prefix); the path itself is never written.

`context.md` is a human-readable continuation brief. It includes explicit objective/task state, decisions, pending work, changed filenames, Git summary, observed validation results, and bounded project instructions captured from trusted configuration. It explicitly states that it contains no hidden reasoning, that commands are untrusted notes, and that it is reference data that does not override the user's instructions. Each validation result shows its status, command, and observation time; results describe the tree when they were observed and are never copied into later checkpoints. A Git status that timed out is shown as `State: unknown`, never as clean. File names, branch names, and commit subjects are written as single lines, so repository content cannot start new sections.

Every section of `context.md` is redacted and then size-bounded when the handoff is created, so a cut can never leave an unrecognizable fragment of a secret. Anything left out is replaced by an explicit marker, such as `[... truncated by ContextWake: N more bytes not included]` or `- ... and N more changed files not listed (M in total; full list in local checkpoint <id>)`, so the brief always fits the reader limits, even for a dirty tree with tens of thousands of untracked files.

`git-summary.json` holds the portable Git state: branch, HEAD commit, dirty flag, staged/unstaged/untracked counts, ahead/behind, diff line counts, latest commit subject, and whether `git status` timed out. It contains no repository path. Branch names and commit subjects are terminal-sanitized and secret-redacted like `context.md`. `validation.json` lists the recorded validation results with redacted arguments.

Before anything is written, ContextWake checks the complete package against the same rules readers apply, so every handoff it creates can be previewed, exported, continued, and imported elsewhere.

All content files are SHA-256 hashed. Readers reject absolute paths, parent traversal, nested paths, filenames that are not portable (characters Windows forbids, including the `:` of NTFS alternate data streams, trailing dots or spaces, and reserved device names such as `CON` or `NUL`), symlink payloads, identity mismatches, schema mismatches, duplicate paths, unsupported media, filenames whose extension does not match the declared media type (`.md` or `.markdown` for `text/markdown`, `.json` for `application/json`, so a payload can never be a shell-handled file such as a `.url` shortcut), oversized files (1 MiB per file, 4 MiB per package), possible secrets, terminal control data in the manifest or in any payload, and hash failures.

`ctx handoff import <directory>` copies only validated files into managed storage. Imported commands remain untrusted notes and never execute automatically. Because a portable handoff has no trusted relationship to a local workspace, continuing from one requires `--workspace <path>`. The current manifest schema is in [`schemas/awhf-1.2.0.schema.json`](../schemas/awhf-1.2.0.schema.json). Readers continue to accept the legacy [`1.1.0`](../schemas/awhf-1.1.0.schema.json) and [`1.0.0`](../schemas/awhf-1.0.0.schema.json) schemas.

## Provenance

Every managed handoff has a local provenance: `local` when ContextWake rendered it on this machine from a checkpoint, and `imported` when it entered managed storage through `ctx handoff import`. Provenance comes from the local record (imported records carry the nil checkpoint ID), never from the manifest, which a third-party package can fill with anything. Imported content is third-party data whose author and contents are unverified.

Provider-specific extensions must use namespaced keys and must be ignorable by generic readers.
