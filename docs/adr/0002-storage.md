# ADR 0002: SQLite metadata and versioned artifacts

- Status: Accepted
- Date: 2026-09-10

## Decision

Store queryable local metadata in bundled SQLite and store checkpoints/handoffs as versioned, inspectable files. Keep credentials out of both.

## Rationale

Profiles, workspaces, sessions, and usage events require filtering and transactional updates. SQLite supplies those properties without a service. Checkpoints and handoffs benefit from portable JSON/Markdown, content hashes, and independent schema evolution.

Schema v3 normalized `agent_id`, `model_provider_id`, and `model`; schema v4 adds sanitized provider session titles. Migrations preserve v1/v2/v3 state. Managed artifact reads enforce canonical containment, file bounds, and symlink rejection.

## Consequences

SQLite is local-only and is not a synchronization layer. Export/import is the deliberate portability mechanism. Backup/recovery tooling remains future work.
