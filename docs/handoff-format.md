# Agent Workspace Handoff Format (AWHF) 1.1.0

The alpha writes a directory package:

```text
handoff/
  manifest.json
  context.md
  git-summary.json
  validation.json
```

`manifest.json` contains the schema version, UUID, UTC creation time, source tool, structured source agent/model-provider/model/profile/session metadata, checkpoint reference, a workspace path fingerprint, branch/commit, objective, content hashes, redaction counts, and security flags. Agent, model provider, and model are deliberately separate concepts.

`context.md` is a human-readable continuation brief. It includes explicit objective/task state, decisions, pending work, changed filenames, Git summary, observed validation results, and bounded project instructions captured from trusted configuration. It explicitly states that it contains no hidden reasoning and that commands are untrusted notes.

All content files are SHA-256 hashed. Readers reject absolute paths, parent traversal, symlink payloads, identity mismatches, schema mismatches, duplicate paths, unsupported media, oversized files, possible secrets, terminal control data, and hash failures.

`adeck handoff import <directory>` copies only validated files into managed storage. Imported commands remain untrusted notes and never execute automatically. Because a portable handoff has no trusted relationship to a local workspace, continuing from one requires `--workspace <path>`. The current manifest schema is in [`schemas/awhf-1.1.0.schema.json`](../schemas/awhf-1.1.0.schema.json). Readers continue to accept the legacy [`1.0.0 schema`](../schemas/awhf-1.0.0.schema.json).

Provider-specific extensions must use namespaced keys and must be ignorable by generic readers.
