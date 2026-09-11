# Gemini CLI agent

Evidence and installed behavior were revalidated on 11 September 2026 against Gemini CLI 0.59.0, its official documentation, and its official source repository. The CLI was installed from the pinned `@google/gemini-cli@0.59.0` npm package on Windows; no user authentication was initiated.

| Capability | Classification | AgentDeck status |
|---|---|---|
| Detect executable/version | VERIFIED | Real Windows package detected as 0.59.0; current `bundle/gemini.js` and the legacy entry point are supported without invoking an npm shell wrapper |
| Authentication flow | PARTIAL | Provider-owned interactive selector can be launched; it requires deliberate user interaction |
| Authentication status/logout | UNSUPPORTED | No supported non-interactive status or logout command was verified; the adapter reports `Unknown` |
| Session listing | PARTIAL | `--list-sessions` is documented but human-readable; AgentDeck does not parse it |
| Native continuation | PARTIAL | Same-profile `--resume <UUID>` construction is implemented; authenticated live resume QA is pending |
| Model selection | VERIFIED | `--model` is exposed and argument construction is tested |
| Account-specific model catalog | UNSUPPORTED | Documented aliases are not presented as a live account catalog |
| Profile isolation | PARTIAL | `GEMINI_CLI_HOME` is documented and exercised, but OAuth/keychain isolation still needs authenticated cross-platform QA |
| Programmatic output | VERIFIED | Headless JSON and stream-JSON are exposed by the installed CLI |
| Provider quota usage | UNSUPPORTED | No quota value is displayed |
| Portable AWHF | VERIFIED BY CONTRACT | Starts with `--prompt-interactive`, an explicit include directory, extensions disabled, and default approval mode; authenticated launch QA is pending |

AgentDeck creates `<profile-home>/.gemini/settings.json` containing only a schema reference and `general.enableAutoUpdate=false`. It stores no API keys. `GEMINI_CLI_HOME` points at the UUID-scoped profile root because Gemini appends its own `.gemini` directory.

An isolated, unauthenticated invocation returned Gemini's structured authentication-required error with exit code 41. This verifies the boundary without borrowing the developer's global login. Gemini launches use `--extensions none` and `--approval-mode default`; AgentDeck never passes `--yolo` or `--skip-trust`.

On Windows, Gemini 0.59.0 exposes `bundle/gemini.js`. AgentDeck searches for a trusted PATH `node.exe` and that package entry point, with `dist/index.js` retained only for older packages. A native `gemini.exe` remains preferred if one exists. Repository-local launchers are rejected.

## Official references

- [Gemini CLI repository and installation](https://github.com/google-gemini/gemini-cli)
- [Gemini CLI reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md)
- [Session management](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md)
- [Configuration and `GEMINI_CLI_HOME`](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md)
- [Authentication](https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/authentication.mdx)
- [Official npm package manifest](https://github.com/google-gemini/gemini-cli/blob/main/packages/cli/package.json)
