# Release process

The current alpha release is `v0.1.0-alpha.3`; its immutable predecessors
`v0.1.0-alpha.2` and `v0.1.0-alpha.1` remain available. Alpha.2 introduced the
expanded nine-agent registry; alpha.3 adds the direct installation and public
launch surface.
Current archives contain canonical `ctx` plus the compatibility alias. Public
publication remains an explicit maintainer action.

## Local gates

Run the final locked format, Clippy, test, release-build, RustSec audit, and Cargo package checks from a clean tree. Exercise the native binary outside the repository before producing archives.

`scripts/package-release.ps1` accepts an explicit Rust target and safe label, stages only `ctx`/`ctxwake`, README, and the Apache-2.0 license, then creates a platform archive under ignored `dist/`. It reads the binaries from `target/<target>/release` (a `cargo build --target <target>` build); pass `-BinaryDirectory target/release` to package a host build made without `--target`. Zip entries always use `/` separators. Unix archives must be packaged on a Unix host so executable mode bits survive; the script deliberately refuses to create them on Windows. `scripts/write-checksums.ps1` generates `SHA256SUMS` for those archives with LF line endings on every platform, because `scripts/install.sh` rejects CRLF manifests. Both scripts resolve relative paths against the current PowerShell location and find the repository from their own location, so they work from any directory.

On Windows, check the archive with the installer regression suite before publishing it:

```powershell
cargo build --locked --release
./scripts/check-windows-runtime.ps1 -Path target/release/ctx.exe, target/release/ctxwake.exe
./scripts/package-release.ps1 -Target x86_64-pc-windows-msvc -Label windows-x86_64 -BinaryDirectory target/release
./scripts/test-windows-installer.ps1 -Archive dist/contextwake-v0.1.0-alpha.3-windows-x86_64.zip
```

Run the suite with both `powershell.exe` (Windows PowerShell 5.1) and `pwsh` (PowerShell 7), as CI does. It needs `cargo` on `PATH`. It installs into a temporary directory from a temporary local release mirror. PATH changes go to a disposable `HKCU\Software\ContextWakeInstallerTests\<id>` key; the real `HKCU\Environment` PATH is only read. Before any case runs the scripts for real, the suite checks that every PATH write in them goes to the key the test hook selects, statically and by running both scripts' PATH code against a test key. It compares the real PATH with its starting value after every case and child process, stops at the first change, and prints the original value to restore. It covers:

- PATH round trips (`REG_EXPAND_SZ`, `REG_SZ`, a missing value, an entry the user already had, a respelled entry the user added);
- an unsupported PATH value type and `uninstall.ps1 -NoPathUpdate`;
- rollback after a binary fails verification post-swap, and a previous binary that cannot be restored surviving later installs;
- updating and uninstalling while `ctx.exe` is running;
- rejection of relative install directories, checksum mismatches, and unexpected archive entries;
- the README scriptblock invocation;
- the checksum, packaging, and runtime-check scripts, the action pins, and the release workflow gates.

## Windows runtime

`.cargo/config.toml` links the C runtime statically for `x86_64-pc-windows-msvc` (`-C target-feature=+crt-static`), so `ctx.exe` and `ctxwake.exe` import only DLLs that ship with Windows and start on a clean system without the Visual C++ Redistributable. Releases up to and including `v0.1.0-alpha.3` were built before this change and import `VCRUNTIME140.dll`. The setting does not affect GNU or Unix targets. A `RUSTFLAGS` environment variable replaces it entirely, so CI and the release workflow run `scripts/check-windows-runtime.ps1` on the built executables. That script reads the import tables without running the files, and fails on Visual C++ or MinGW runtime DLLs.

## CI

The ordinary CI matrix validates Ubuntu, Windows, and macOS. The release workflow builds native archives for Linux x86_64, Windows x86_64 MSVC, and macOS Apple Silicon. A pushed tag must exactly match the Cargo version, for example `v0.1.0-alpha.3`; tag-triggered publication creates a GitHub prerelease using the matching checked-in release notes. Manual dispatch builds artifacts but does not publish.

The release workflow only publishes builds that pass every gate:

- Each build job compiles from a clean checkout of the tagged commit and restores no build cache.
- It runs `cargo fmt --check`, Clippy with `-D warnings`, and the full test suite before the release build.
- The Windows job also runs the runtime check and the installer regression suite against the packaged archive, with both PowerShell editions.
- `publish` needs every build job and the checksum job, so a commit that fails any gate publishes nothing.
- `SHA256SUMS` is written by `scripts/write-checksums.ps1`.

`installers.yml` runs the same suite against a package built from each pushed commit. It also exercises the current scripts against the live published release, including a raw-registry PATH round trip on the runner.

Every third-party action is pinned to a full commit SHA, with the tag it was resolved from in a trailing comment, so Dependabot (`.github/dependabot.yml`) can track every pin; the installer regression suite fails on a pin without a version-tag comment. `dtolnay/rust-toolchain` publishes a single moving `v1` tag, which points at the same commit as its `master` branch, and needs the explicit `toolchain` input. If Dependabot does not propose an update after `v1` moves, resolve it with `git ls-remote https://github.com/dtolnay/rust-toolchain refs/tags/v1`, review the diff, and replace the SHA in every workflow. To resolve a tag, use `git ls-remote https://github.com/<owner>/<repo> 'refs/tags/<tag>*'`. For an annotated tag, pin the commit from the `^{}` line.

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

On Windows:

- **PATH.** The installer reads the user PATH raw, with `%VARIABLE%` references unexpanded, from `HKCU\Environment`. It appends only the install directory and writes the value back with its original registry type (`REG_EXPAND_SZ` stays `REG_EXPAND_SZ`). Then it broadcasts `WM_SETTINGCHANGE` so new terminals see the change. If the directory is already present, compared case-insensitively and ignoring trailing separators, PATH is left untouched. The `.contextwake-path-added` marker records that the installer added the entry. `scripts/uninstall.ps1` removes only that entry, the one spelled exactly as the installer appended it, so every other entry survives byte for byte, including the user's own `%VARIABLE%` spelling of the same directory. A PATH value of any type other than `REG_EXPAND_SZ` or `REG_SZ` stops both scripts before they change anything; `-NoPathUpdate` skips the PATH step. For uninstall, the entry and its marker then stay, so a later uninstall without the switch removes both.
- **Install directory.** `-InstallDir` and `CONTEXTWAKE_INSTALL_DIR` must be absolute paths such as `C:\Tools\ContextWake`. Relative, drive-relative, and root-relative paths are rejected before anything is written.
- **Updating while running.** Windows can rename a running executable but cannot delete it. When you update while `ctx` is running, the old binary is renamed aside and the update succeeds. The leftover file is deleted by the next install or uninstall after that process exits. Uninstalling while `ctx` is running works the same way: rerun it later to delete the leftover and the empty directory.
- **Failed updates.** A new binary that fails its version check is removed and the previous binaries are restored. The error reports the binary's exit code and output. If a previous binary cannot be restored, it is kept as `.<name>.<id>.previous.exe` and the warning names that file. Later installs never delete it; uninstall removes it with the other binaries.

Test hooks:

| Variable | Effect |
|---|---|
| `CONTEXTWAKE_TEST_RELEASE_DIR` | Reads release files from a local mirror of GitHub's `releases/download/<tag>/` layout. The installer prints a warning. |
| `CONTEXTWAKE_TEST_ENVIRONMENT_KEY` | Redirects PATH updates to a key below `HKCU\Software\ContextWakeInstallerTests`. Other keys are refused. |
| `CONTEXTWAKE_INSTALLER_SOURCE_ONLY=1` | Defines the functions of `install.ps1` or `uninstall.ps1` without running them. |
| `CONTEXTWAKE_TEST_CORRUPT_ARCHIVE` | Existing negative-test hook. |
| `CONTEXTWAKE_TEST_ARCHITECTURE` | Existing negative-test hook. |

README commands use an immutable commit URL for installer code. A release does
not need a new binary merely because the repository adds or updates installation
tooling compatible with its existing archive layout. Any future archive-layout
change must update installer validation and may require a new release.

The one-liners in `README.md` and `docs/releases/v0.1.0-alpha.3.md` still pin
commit `28922ba`. Its `install.ps1` predates the Windows fixes above: it rewrites
the user PATH with `%VARIABLE%` entries expanded and as `REG_SZ`, resolves a
relative `-InstallDir` against the process directory, and reports a failed
update while `ctx` is running. The README therefore does not describe those
fixes yet. Once a commit
that contains them is public, re-pin every one-liner to it. Then state in the
README's Windows section that the installer leaves every other PATH entry,
including unexpanded `%VARIABLE%` references, unchanged, and that updating
works while `ctx` is running.
