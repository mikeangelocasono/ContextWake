# Release process

The current alpha release is `v0.1.0-alpha.2`; its immutable predecessor
`v0.1.0-alpha.1` remains available with its historical `ctxwake` executable.
Current archives contain canonical `ctx` plus the compatibility alias. Public
publication remains an explicit maintainer action.

## Local gates

Run the final locked format, Clippy, test, release-build, RustSec audit, and Cargo package checks from a clean tree. Exercise the native binary outside the repository before producing archives.

`scripts/package-release.ps1` accepts an explicit Rust target and safe label, stages only `ctx`/`ctxwake`, README, and the Apache-2.0 license, then creates a platform archive under ignored `dist/`. Unix archives must be packaged on a Unix host so executable mode bits survive; the script deliberately refuses to create them on Windows. `scripts/write-checksums.ps1` generates `SHA256SUMS` for those archives.

## CI

The ordinary CI matrix validates Ubuntu, Windows, and macOS. The release workflow builds native archives for Linux x86_64, Windows x86_64 MSVC, and macOS Apple Silicon. A pushed tag must exactly match the Cargo version, for example `v0.1.0-alpha.2`; tag-triggered publication creates a GitHub prerelease. Manual dispatch builds artifacts but does not publish.

## Signing and notarization

No signing identity is currently available. Local and CI-produced alpha binaries are therefore **unsigned**:

- Windows production signing requires an Authenticode code-signing certificate whose private key is protected in a suitable signing service or hardware-backed store. The CI design must use short-lived authorization; a PFX must not be committed.
- macOS distribution needs an Apple Developer ID Application identity and notarization credentials, followed by stapling. CI compilation is not equivalent to signing, notarization, or physical-device QA.
- Linux archives are checksum-protected but not signed. A future release may sign `SHA256SUMS` with Sigstore or a maintained signing key.

Users should compare an archive against the published SHA-256 manifest. Documentation must not suggest disabling Defender, Gatekeeper, or other platform protection to run an unsigned build.

## Direct installers

Repository-provided `scripts/install.ps1` and `scripts/install.sh` install the
existing release archives into user-local binary directories. They resolve the
latest compatible public release (including prereleases), verify `SHA256SUMS`,
allow only the expected archive entries, and verify both CLI binaries before
reporting success. Rerunning an installer is the supported alpha update path.

README commands use an immutable commit URL for installer code. A release does
not need a new binary merely because the repository adds or updates installation
tooling compatible with its existing archive layout. Any future archive-layout
change must update installer validation and may require a new release.
