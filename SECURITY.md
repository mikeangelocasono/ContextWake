# Security Policy

## Supported versions

The project is pre-release. Security fixes apply to the latest source revision
until a public release-support policy is published.

## Report a vulnerability

Use [GitHub's private vulnerability reporting flow](https://github.com/mikeangelocasono/ContextWake/security)
from the repository Security tab. Do not file a public issue containing a proof
of concept, credentials, private paths, repository names, raw auth files, or
unredacted diagnostic output. If private reporting is unavailable, contact the
maintainer privately through GitHub before disclosing details.

Include the affected revision, operating system, impact, minimal reproduction,
and whether secrets may have been exposed. Replace all real secrets with test
values.

## Scope

Credential leakage, cross-profile isolation failures, command injection,
malicious project configuration, path traversal, symlink attacks, terminal
escape injection, poisoned handoff handling, update integrity, and dependency
compromise are security issues.

Automatic account rotation or provider-limit circumvention will not be accepted
as features.
