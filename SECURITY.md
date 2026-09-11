# Security Policy

## Supported versions

The project is pre-release. Security fixes apply to the latest source revision until a public release policy is published.

## Report a vulnerability

Use the repository host’s private security-advisory feature. If that is not configured, contact the maintainer privately before disclosing details. Do not file a public issue containing a proof of concept, credentials, private paths, repository names, raw auth files, or unredacted diagnostic output.

Include the affected revision, operating system, impact, minimal reproduction, and whether secrets may have been exposed. Replace all real secrets with test values.

## Scope

Credential leakage, cross-profile isolation failures, command injection, malicious project configuration, path traversal, symlink attacks, terminal escape injection, poisoned handoff handling, update integrity, and dependency compromise are security issues.

Automatic account rotation or provider-limit circumvention will not be accepted as features.
