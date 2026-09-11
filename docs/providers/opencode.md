# OpenCode agent

Evidence was revalidated on 11 September 2026 against the official OpenCode documentation, official source, and installed OpenCode 1.18.25. The adapter is registered in AgentDeck.

| Capability | Classification | AgentDeck status |
|---|---|---|
| Detect executable/version | CONFIRMED | Implemented with a bounded `opencode --version` probe |
| Provider credential status | PARTIALLY CONFIRMED | `opencode auth list` is parsed as credential presence; local backends may require no credential |
| Provider login | CONFIRMED | Provider-owned `opencode auth login`; AgentDeck never reads the credential file |
| Provider logout | UNSUPPORTED AS PROFILE-WIDE ACTION | OpenCode requires a particular provider ID; AgentDeck refuses to delete every provider credential implicitly |
| Session listing | CONFIRMED | JSON metadata is imported explicitly by `adeck session sync` and filtered to the canonical workspace |
| Native continuation | CONFIRMED | Same-profile `opencode --session <id>` implemented; authenticated live-resume QA remains outstanding |
| Model selection/catalog | CONFIRMED | Dynamic `provider/model` catalog listing and exact selection validation implemented |
| Multiple/custom/local providers | CONFIRMED | Static examples plus arbitrary safe custom provider IDs; concrete models are validated by OpenCode |
| Usage | PARTIAL | `opencode stats` is local session token/cost observation, not provider quota; not integrated yet |
| Portable AWHF | IMPLEMENTED | Fixed-argument new-session launch references validated `context.md`; command construction has a contract test |
| Profile isolation | EXPERIMENTAL | AgentDeck sets dedicated XDG config/data/cache and `OPENCODE_CONFIG_DIR` roots; locally verified on Windows, not yet cross-platform credential-tested |

OpenCode demonstrates why Coding Agent and Model Provider are separate objects: one coding agent can route to many hosted, custom, and local backends. AgentDeck stores the normalized provider and model separately even when the CLI accepts `provider/model`.

AgentDeck starts OpenCode with external plugins disabled (`--pure`), automatic sharing disabled, automatic updating/pruning disabled, and terminal-title changes disabled. These controls reduce ambient mutation and plugin risk; they do not sandbox the coding agent.

`adeck session sync` reads only the official public JSON fields (`id`, `title`, timestamps, and directory). It does not read OpenCode's SQLite database or transcript content. Empty output is accepted as an empty session list because this is observed behavior for a fresh isolated profile.

## Official references

- [OpenCode CLI reference](https://opencode.ai/docs/cli/)
- [OpenCode configuration](https://opencode.ai/docs/config/)
- [OpenCode providers](https://opencode.ai/docs/providers/)
- [Official session-list JSON implementation](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/session.ts)
- [Official model-list implementation](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/models.ts)
