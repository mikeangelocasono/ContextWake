# Design Contract

## Direction

AgentDeck is a compact continuity console, not a branded splash screen. The
dominant hierarchy is active identity, current workspace/Git evidence, and the
continuity action. Dense tables support browsing; warning dialogs slow down
identity changes without making routine inspection cumbersome.

## Visual language

- Graphite terminal defaults with one restrained cyan accent.
- Amber warnings and explicit PASS/WARN/FAIL labels; color never carries status alone.
- One-cell borders, compact spacing, and no decorative ASCII art.
- Wide terminals use a profile rail plus workspace/session content. Narrow
  terminals collapse to one column. Very small terminals show essential state
  and a clear size warning.

## Interaction

- Global mnemonic keys change screens; `?` opens the complete key reference.
- `P` opens profiles, `I` opens agent capabilities, `C` opens checkpoints, and
  `O` opens handoffs. Screen-specific actions remain visible in panel titles.
- Destructive and identity-changing operations require confirmation.
- The switch dialog says “checkpoint + handoff” and never describes a new
  session as a native resume.

## Trust and content

Every visible value comes from SQLite, Git, the local filesystem, or a supported
coding-agent command. Unknown agent/provider values remain visibly unavailable. All
repository/agent text is stripped of terminal control sequences before
rendering.

## Responsive states

The render contract covers wide, narrow, and tiny terminals, plus onboarding,
empty collections, provider unavailable, warnings, errors, confirmations, and
resizing. Tests render fixed buffers at representative dimensions.
