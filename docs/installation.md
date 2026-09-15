# Installing ContextWake

ContextWake provides user-local installers backed by official GitHub Release
archives. Normal installation does not require Git, Rust, administrator access,
or a repository clone.

The commands below download an immutable, commit-pinned installer. The installer
selects the newest compatible public release, including prereleases while
ContextWake is in alpha. If release discovery is temporarily unavailable, this
installer falls back to the known-good `v0.1.0-alpha.2` release.

## Supported prebuilt platforms

| Platform | Architecture | Install location |
|---|---|---|
| Windows | x86_64 | `%LOCALAPPDATA%\ContextWake\bin` |
| Linux | x86_64 | `~/.local/bin` |
| macOS | Apple Silicon (`arm64`) | `~/.local/bin` |

Other platforms can [build from source](../README.md#build-from-source). There
is currently no prebuilt Windows ARM64, Linux ARM64, or Intel macOS archive.

## Windows PowerShell

Install:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.ps1)))
```

The script supports Windows PowerShell 5.1 and PowerShell 7. It downloads the
release ZIP and `SHA256SUMS`, verifies the archive before extraction, installs
`ctx.exe` and `ctxwake.exe`, and adds the installation directory to the current
user's `PATH` without changing the system `PATH`.

To inspect the installer first:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.ps1 -OutFile install-contextwake.ps1
Get-Content .\install-contextwake.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\install-contextwake.ps1
```

`-ExecutionPolicy Bypass` applies only to that child process. It does not change
the user's or machine's execution policy.

Install a specific version or directory:

```powershell
.\install-contextwake.ps1 -Version v0.1.0-alpha.2 -InstallDir C:\Tools\ContextWake
```

Environment variables work with the one-line installer too:

```powershell
$env:CONTEXTWAKE_VERSION = 'v0.1.0-alpha.2'
$env:CONTEXTWAKE_INSTALL_DIR = 'C:\Tools\ContextWake'
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.ps1)))
```

Custom directories are not added to `PATH` automatically unless the PowerShell
installer is allowed to update the user `PATH`. Pass `-NoPathUpdate` or set
`CONTEXTWAKE_NO_PATH_UPDATE=1` to skip all PATH changes.

## Windows Command Prompt

Install from CMD:

```bat
powershell -NoProfile -Command "& ([scriptblock]::Create((irm 'https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.ps1')))"
```

Open a new Command Prompt if the installer added the installation directory to
the user `PATH`. No execution-policy or security setting is changed globally.

## Linux and macOS

Install:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.sh | sh
```

The script requires standard platform tools: `curl`, `tar`, `awk`, `grep`,
`sed`, and either `sha256sum` or `shasum`. It installs `ctx` and `ctxwake` to
`~/.local/bin`. When that directory is missing from `PATH`, it adds one marked,
idempotent entry to `.bashrc`, `.zshrc`, or `.profile`. Open a new terminal after
that change.

To inspect the installer first:

```sh
curl --proto '=https' --tlsv1.2 -fsSLo install-contextwake.sh https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/install.sh
less install-contextwake.sh
sh install-contextwake.sh
```

Install a specific version or directory:

```sh
CONTEXTWAKE_VERSION=v0.1.0-alpha.2 \
CONTEXTWAKE_INSTALL_DIR="$HOME/bin" \
sh install-contextwake.sh
```

Set `CONTEXTWAKE_NO_PATH_UPDATE=1` to prevent shell-profile changes. Custom
installation directories are reported but are not written into a shell profile.

The macOS archive supports Apple Silicon only. It is CI validated, unsigned,
and not notarized. Keep Gatekeeper enabled; the installer does not alter it.

## Verify and start

After installation, open a new terminal if instructed and run:

```sh
ctx --version
ctx doctor
ctx
```

`ctx doctor` checks ContextWake, Git, local state, and the supported AI coding
agent installations. ContextWake installs no coding agent and owns no provider
credentials.

## Reinstall or update

Rerun the same install command. Installation is idempotent: binaries are safely
replaced, PATH entries are not duplicated, and ContextWake user/project state is
untouched. During the alpha, rerunning the installer is the supported update
mechanism; there is no `ctx update` command.

## Uninstall

Windows PowerShell:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/uninstall.ps1)))
```

Linux or macOS:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/mikeangelocasono/ContextWake/28922bab55ae49f95ef25a8381fb7a70e0e99d3c/scripts/uninstall.sh | sh
```

Uninstall removes only `ctx`, `ctxwake`, and a PATH entry previously owned by
the installer. It preserves profiles, workspaces, sessions, checkpoints,
handoffs, configuration, databases, and `.contextwake` project files.

For a custom directory, set `CONTEXTWAKE_INSTALL_DIR` before invoking the
uninstaller.

## Manual archive installation

The current release archives and checksum manifest remain available on the
[`v0.1.0-alpha.2` release page](https://github.com/mikeangelocasono/ContextWake/releases/tag/v0.1.0-alpha.2).
Verify the archive against `SHA256SUMS` before extraction.

## Package-manager status

| Channel | Status | Notes |
|---|---|---|
| Direct GitHub installer | Available | Primary user installation path |
| Manual GitHub archive | Available | Checksum-protected release assets |
| Cargo / crates.io | NOT YET AVAILABLE | The crate is not published |
| Winget | FUTURE | No ContextWake package or signed installer submission yet |
| Scoop | NOT YET AVAILABLE | The current Windows ZIP can support a future manifest |
| Chocolatey | FUTURE | No package has been published |
| Homebrew / Linuxbrew | NOT YET AVAILABLE | Formula submission and broader macOS coverage remain future work |

No package-manager registry publication is part of `v0.1.0-alpha.2`.

## Installer security

- Downloads use HTTPS from the official ContextWake GitHub repository only.
- Release tags and filenames must match strict expected formats.
- Redirects remain HTTPS and are restricted to GitHub-owned hosts.
- SHA-256 verification is mandatory and fails closed.
- Archives must contain only the expected release directory, binaries, README,
  and license; links and unexpected paths are rejected.
- Installation uses temporary files and restores an earlier installation if
  final binary verification fails.
- No binary runs before its archive checksum passes.
- No administrator or `sudo` privilege is requested.
- Binary installation and ContextWake user/project state remain separate.

Windows and macOS alpha binaries are not code-signed, and macOS binaries are not
notarized. SHA-256 protects release integrity but is not a replacement for
platform code signing. Keep Windows security and macOS Gatekeeper enabled.
