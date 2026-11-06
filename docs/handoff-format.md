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

## Conversation excerpt

Each coding agent keeps its own conversation history; ContextWake never merges or transfers native histories. To tell the next agent what the user last asked for, a checkpoint (schema v3, [`schemas/checkpoint-v3.schema.json`](../schemas/checkpoint-v3.schema.json); readers accept v1 to v3) can store a `conversation` excerpt read from the source agent's own local transcript for the workspace:

- `agent_id`, `provider_session_id`, optional `session_title`, `captured_at`, optional `last_activity_at`;
- `user_requests`: at most the 5 most recent typed user requests, oldest first, each at most 600 characters;
- `last_agent_message`: the agent's latest final answer, at most 3,000 characters;
- `truncated`: whether older messages or longer text were left out.

Only typed user requests and final answers are read: never tool calls or tool output, hidden reasoning, or system, developer and injected context. Every text is terminal-sanitized and secret-redacted by the adapter and again when the checkpoint is written. `ctx checkpoint create` and switch checkpoints (`ctx profile use P --handoff`) capture the excerpt from the active (source) profile's agent; `--no-conversation` or `capture_conversation_excerpts = false` in `config.toml` turns this off. A transcript that cannot be read produces a warning and never blocks the checkpoint. A switch checkpoint seeded from an earlier checkpoint copies its project state but never its excerpt.

In `context.md` the excerpt is the section `## Recent Conversation (excerpt from <Agent>, session <id>, captured <time>)`. Requests are list entries and the answer is a block quote, so transcript text cannot start a new section.

## Launch prompt

`ctx handoff continue` and `ctx profile use P --handoff --launch` give the agent one first message built by ContextWake (not by each adapter) from the validated handoff:

1. The delivery marker line `ContextWake handoff <handoff-id> · launch <8-hex nonce>`; every launch has a new nonce.
2. The untrusted-data framing for the handoff's provenance (local or imported).
3. Whether the agent resumes its own earlier session ("work may have continued in another coding agent since your last turn; the state below is current") or starts a new one.
4. Inline sections in priority order: Objective, Current work, Pending, Completed, Decisions, Known issues, Recent conversation (labelled as an excerpt from the source agent), Git state with at most 50 changed files. An imported handoff contributes only its manifest objective; its details stay in `context.md`.
5. The absolute path of the full `context.md`, the rule that commands are notes only, a request to begin with a one-paragraph summary of where the work stands and the next step, and how to save progress for the next agent (`ctx checkpoint create --objective ... --completed ... --decision ... --pending ...`, after asking the user).

The prompt is at most 12,000 characters (Windows limits a whole command line to 32,767 characters). Lower sections are cut or left out first, with an explicit `[… omitted; see context.md]` marker naming what was left out; the header and the rules are always kept. All recorded text is redacted, the final prompt is scanned again, line endings are LF, and the prompt never starts with `-`.

After the agent exits, ContextWake looks in the agent's own local session store for the session the launch created or resumed and for a user message containing the marker. Only that match counts as delivery: the session is then recorded as `resumed_with_handoff` ("Native Resume + Handoff") when it is the session that was to be resumed, or `restored_from_handoff` when it is a new session (including an attempted resume that the agent turned into a new session, which ContextWake reports as such). Otherwise it is recorded as `unknown` and ContextWake says that it could not confirm that the agent received the handoff, and why: either the session's transcript has no message with the marker, or the agent's local session store did not show which session the launch used (for example because ContextWake cannot read that agent's transcripts), so no transcript was checked.
