#!/bin/sh
set -eu

repository="mikeangelocasono/ContextWake"
repository_url="https://github.com/$repository"
release_feed_url="$repository_url/releases.atom"
default_release_version="v0.1.0-alpha.3"
path_marker_name=".contextwake-path-added"
path_line='export PATH="$HOME/.local/bin:$PATH" # ContextWake installer'

fail() {
    printf 'ContextWake installation failed: %s\n' "$1" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command is unavailable: $1"
}

valid_version() {
    printf '%s\n' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$'
}

trusted_effective_url() {
    case "$1" in
        https://github.com/* | https://api.github.com/* | https://*.githubusercontent.com/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

perform_download() {
    download_url=$1
    download_output=$2
    download_effective_url=$(curl \
        --proto '=https' \
        --proto-redir '=https' \
        --tlsv1.2 \
        --fail \
        --silent \
        --show-error \
        --location \
        --max-redirs 5 \
        --connect-timeout 15 \
        --max-time 300 \
        --retry 2 \
        --header 'Accept: application/vnd.github+json' \
        --header 'User-Agent: ContextWake-Installer' \
        --output "$download_output" \
        --write-out '%{url_effective}' \
        "$download_url") || return 1
    trusted_effective_url "$download_effective_url"
}

trusted_download_url() {
    case "$1" in
        https://github.com/* | https://api.github.com/*) return 0 ;;
        *) return 1 ;;
    esac
}

download() {
    download_url=$1
    download_output=$2
    trusted_download_url "$download_url" || fail "refusing an untrusted download URL"
    perform_download "$download_url" "$download_output" ||
        fail "download failed or redirected to an unexpected host: $download_url"
}

try_download() {
    download_url=$1
    download_output=$2
    trusted_download_url "$download_url" || return 1
    perform_download "$download_url" "$download_output"
}

manifest_checksum() {
    manifest_file=$1
    manifest_asset=$2
    awk -v asset="$manifest_asset" '
        NF == 2 && $2 == asset && length($1) == 64 && $1 !~ /[^0-9A-Fa-f]/ {
            print tolower($1)
        }
    ' "$manifest_file"
}

resolve_latest_version() {
    release_feed=$1
    asset_label=$2
    manifest_destination=$3

    release_tags=""
    if try_download "$release_feed_url" "$release_feed"; then
        release_size=$(wc -c < "$release_feed" | tr -d ' ')
        [ "$release_size" -le 524288 ] || fail "GitHub release metadata exceeded the 512 KiB safety limit"
        release_tags=$(sed -n "s|.*$repository_url/releases/tag/\(v[0-9A-Za-z.-]*\).*|\1|p" "$release_feed" | awk '!seen[$0]++')
    else
        printf 'Latest-release lookup failed; trying the installer pinned known-good release.\n' >&2
    fi
    case " $release_tags " in
        *" $default_release_version "*) ;;
        *) release_tags="$release_tags $default_release_version" ;;
    esac

    for release_tag in $release_tags; do
        valid_version "$release_tag" || continue
        candidate_archive="contextwake-$release_tag-$asset_label"
        case "$asset_label" in
            windows-*) candidate_archive="$candidate_archive.zip" ;;
            *) candidate_archive="$candidate_archive.tar.gz" ;;
        esac
        candidate_manifest="$temporary_directory/SHA256SUMS.$release_tag"
        if try_download "$repository_url/releases/download/$release_tag/SHA256SUMS" "$candidate_manifest" 2>/dev/null; then
            candidate_hashes=$(manifest_checksum "$candidate_manifest" "$candidate_archive")
            candidate_count=$(printf '%s\n' "$candidate_hashes" | sed '/^$/d' | wc -l | tr -d ' ')
            if [ "$candidate_count" -eq 1 ]; then
                cp "$candidate_manifest" "$manifest_destination"
                printf '%s\n' "$release_tag"
                return 0
            fi
        fi
    done
    fail "no compatible public ContextWake release was found for $operating_system/$machine_architecture"
}

validate_archive() {
    archive_file=$1
    archive_root=$2
    archive_list="$temporary_directory/archive.list"
    archive_verbose_list="$temporary_directory/archive.verbose.list"

    tar -tzf "$archive_file" > "$archive_list" || fail "release archive could not be listed"
    tar -tvzf "$archive_file" > "$archive_verbose_list" || fail "release archive metadata could not be read"

    archive_count=$(wc -l < "$archive_list" | tr -d ' ')
    [ "$archive_count" -le 16 ] || fail "release archive contains too many entries"
    while IFS= read -r archive_entry; do
        case "$archive_entry" in
            "$archive_root/" | "$archive_root/ctx" | "$archive_root/ctxwake" | "$archive_root/README.md" | "$archive_root/LICENSE")
                ;;
            *)
                fail "release archive contains an unexpected entry: $archive_entry"
                ;;
        esac
    done < "$archive_list"

    for required_entry in \
        "$archive_root/ctx" \
        "$archive_root/ctxwake" \
        "$archive_root/README.md" \
        "$archive_root/LICENSE"; do
        grep -Fx "$required_entry" "$archive_list" >/dev/null ||
            fail "release archive is missing: $required_entry"
    done

    while IFS= read -r verbose_entry; do
        archive_type=$(printf '%.1s' "$verbose_entry")
        case "$archive_type" in
            d | -) ;;
            *) fail "release archive contains a link or unsupported entry type" ;;
        esac
    done < "$archive_verbose_list"
}

binary_matches_version() {
    binary_path=$1
    command_name=$2
    release_version=$3
    expected_output="$command_name ${release_version#v}"
    actual_output=$("$binary_path" --version 2>&1) || return 1
    [ "$actual_output" = "$expected_output" ]
}

verify_binary() {
    binary_path=$1
    command_name=$2
    release_version=$3
    binary_matches_version "$binary_path" "$command_name" "$release_version" ||
        fail "$command_name verification failed; expected '$command_name ${release_version#v}'"
}

rollback_install() {
    for rollback_name in ctx ctxwake; do
        rollback_target="$install_directory/$rollback_name"
        rollback_backup="$temporary_directory/$rollback_name.backup"
        case "$rollback_name" in
            ctx) rollback_had_binary=$had_ctx ;;
            ctxwake) rollback_had_binary=$had_ctxwake ;;
        esac
        if [ -f "$rollback_backup" ]; then
            cp "$rollback_backup" "$rollback_target" || true
            chmod 755 "$rollback_target" || true
        elif [ "$rollback_had_binary" = "0" ]; then
            rm -f "$rollback_target"
        fi
    done
}

install_binaries() {
    payload_directory=$1
    release_version=$2

    [ ! -L "$install_directory" ] || fail "install directory must not be a symbolic link"
    mkdir -p "$install_directory"
    [ -d "$install_directory" ] && [ ! -L "$install_directory" ] ||
        fail "install directory must be a real directory"

    transaction=$$
    had_ctx=0
    had_ctxwake=0
    for install_name in ctx ctxwake; do
        install_source="$payload_directory/$install_name"
        install_target="$install_directory/$install_name"
        install_staged="$install_directory/.$install_name.$transaction.new"
        [ -f "$install_source" ] && [ ! -L "$install_source" ] ||
            fail "expected a regular payload file: $install_name"
        if [ -e "$install_target" ] || [ -L "$install_target" ]; then
            [ -f "$install_target" ] && [ ! -L "$install_target" ] ||
                fail "existing binary path is not a regular file: $install_target"
            case "$install_name" in
                ctx) had_ctx=1 ;;
                ctxwake) had_ctxwake=1 ;;
            esac
            cp "$install_target" "$temporary_directory/$install_name.backup"
        fi
        cp "$install_source" "$install_staged"
        chmod 755 "$install_staged"
    done

    verify_binary "$install_directory/.ctx.$transaction.new" ctx "$release_version"

    install_transaction_active=1
    if ! mv -f "$install_directory/.ctx.$transaction.new" "$install_directory/ctx"; then
        rollback_install
        install_transaction_active=0
        fail "could not replace ctx"
    fi
    if ! mv -f "$install_directory/.ctxwake.$transaction.new" "$install_directory/ctxwake"; then
        rollback_install
        install_transaction_active=0
        fail "could not replace ctxwake"
    fi
    if ! binary_matches_version "$install_directory/ctx" ctx "$release_version" ||
        ! binary_matches_version "$install_directory/ctxwake" ctxwake "$release_version"; then
        rollback_install
        install_transaction_active=0
        fail "installed binary verification failed; the previous installation was restored"
    fi
    install_transaction_active=0
}

select_shell_profile() {
    case "${SHELL:-}" in
        */zsh) printf '%s\n' "$HOME/.zshrc" ;;
        */bash) printf '%s\n' "$HOME/.bashrc" ;;
        *) printf '%s\n' "$HOME/.profile" ;;
    esac
}

configure_path() {
    path_marker="$install_directory/$path_marker_name"
    case ":$PATH:" in
        *":$install_directory:"*)
            printf 'PATH already contains the install directory.\n'
            return 0
            ;;
    esac

    if [ "$install_directory" != "$HOME/.local/bin" ]; then
        printf 'Add this directory to PATH: %s\n' "$install_directory"
        return 0
    fi

    shell_profile=$(select_shell_profile)
    if [ -L "$shell_profile" ]; then
        printf 'Not editing symbolic-link shell profile. Add this line yourself:\n  %s\n' "$path_line"
        return 0
    fi
    if [ -f "$shell_profile" ] && grep -F "$path_line" "$shell_profile" >/dev/null 2>&1; then
        printf 'Shell profile already contains the ContextWake PATH entry.\n'
        return 0
    fi

    printf '\n%s\n' "$path_line" >> "$shell_profile"
    printf '%s\n' "$shell_profile" > "$path_marker"
    printf 'Updated PATH in %s. Open a new terminal before running ctx.\n' "$shell_profile"
}

main() {
    require_command curl
    require_command tar
    require_command awk
    require_command grep
    require_command sed

    operating_system=$(uname -s)
    machine_architecture=$(uname -m)
    if [ -n "${CONTEXTWAKE_TEST_OS:-}" ]; then
        operating_system=$CONTEXTWAKE_TEST_OS
    fi
    if [ -n "${CONTEXTWAKE_TEST_ARCHITECTURE:-}" ]; then
        machine_architecture=$CONTEXTWAKE_TEST_ARCHITECTURE
    fi

    case "$operating_system/$machine_architecture" in
        Linux/x86_64 | Linux/amd64)
            asset_label="linux-x86_64"
            platform_name="Linux x86_64"
            ;;
        Darwin/arm64 | Darwin/aarch64)
            asset_label="macos-aarch64"
            platform_name="macOS Apple Silicon"
            ;;
        Darwin/x86_64 | Darwin/amd64)
            fail "ContextWake does not currently publish a binary for macOS/x86_64. Build from source: $repository_url#build-from-source"
            ;;
        *)
            fail "ContextWake does not currently publish a binary for $operating_system/$machine_architecture. Build from source: $repository_url#build-from-source"
            ;;
    esac

    requested_version=${CONTEXTWAKE_VERSION:-latest}
    if [ "$requested_version" != "latest" ]; then
        valid_version "$requested_version" ||
            fail "version must be 'latest' or a release tag such as v0.1.0-alpha.3"
    fi

    install_directory=${CONTEXTWAKE_INSTALL_DIR:-"$HOME/.local/bin"}
    case "$install_directory" in
        /*) ;;
        *) fail "CONTEXTWAKE_INSTALL_DIR must be an absolute path" ;;
    esac

    temporary_directory=$(mktemp -d "${TMPDIR:-/tmp}/contextwake-install.XXXXXX") ||
        fail "could not create a temporary directory"
    install_transaction_active=0
    transaction=""
    cleanup() {
        if [ "$install_transaction_active" = "1" ]; then
            rollback_install
            install_transaction_active=0
        fi
        if [ -n "$transaction" ]; then
            rm -f \
                "$install_directory/.ctx.$transaction.new" \
                "$install_directory/.ctxwake.$transaction.new"
        fi
        case "$temporary_directory" in
            "${TMPDIR:-/tmp}"/contextwake-install.*)
                [ ! -d "$temporary_directory" ] || rm -rf "$temporary_directory"
                ;;
        esac
    }
    trap cleanup EXIT HUP INT TERM

    manifest_path="$temporary_directory/SHA256SUMS"
    if [ "$requested_version" = "latest" ]; then
        release_feed="$temporary_directory/releases.atom"
        resolved_version=$(resolve_latest_version "$release_feed" "$asset_label" "$manifest_path")
    else
        resolved_version=$requested_version
        download "$repository_url/releases/download/$resolved_version/SHA256SUMS" "$manifest_path"
    fi

    archive_name="contextwake-$resolved_version-$asset_label.tar.gz"
    archive_root="contextwake-$resolved_version-$asset_label"
    archive_path="$temporary_directory/$archive_name"

    printf 'ContextWake Installer\n\n'
    printf 'Platform: %s\n' "$platform_name"
    printf 'Version:  %s\n' "$resolved_version"
    printf 'Install:  %s\n\n' "$install_directory"
    printf 'Downloading release...\n'
    download "$repository_url/releases/download/$resolved_version/$archive_name" "$archive_path"

    if [ "${CONTEXTWAKE_TEST_CORRUPT_ARCHIVE:-0}" = "1" ]; then
        printf 'corrupt' >> "$archive_path"
    fi

    expected_hashes=$(manifest_checksum "$manifest_path" "$archive_name")
    expected_count=$(printf '%s\n' "$expected_hashes" | sed '/^$/d' | wc -l | tr -d ' ')
    [ "$expected_count" -eq 1 ] ||
        fail "SHA256SUMS does not contain exactly one valid checksum for $archive_name"
    expected_hash=$expected_hashes

    if command -v sha256sum >/dev/null 2>&1; then
        actual_hash=$(sha256sum "$archive_path" | awk '{print tolower($1)}')
    elif command -v shasum >/dev/null 2>&1; then
        actual_hash=$(shasum -a 256 "$archive_path" | awk '{print tolower($1)}')
    else
        fail "neither sha256sum nor shasum is available"
    fi
    if [ "$actual_hash" != "$expected_hash" ]; then
        fail "checksum mismatch for $archive_name. Expected: $expected_hash Actual: $actual_hash. Installation aborted."
    fi
    printf 'Verifying SHA-256... OK\n'

    validate_archive "$archive_path" "$archive_root"
    extract_directory="$temporary_directory/extract"
    mkdir "$extract_directory"
    tar -xzf "$archive_path" -C "$extract_directory" || fail "release archive extraction failed"
    payload_directory="$extract_directory/$archive_root"

    printf 'Installing ctx and ctxwake...\n'
    install_binaries "$payload_directory" "$resolved_version"
    printf 'Installing ctx and ctxwake... OK\n'

    if [ "${CONTEXTWAKE_NO_PATH_UPDATE:-0}" != "1" ]; then
        configure_path
    fi

    printf '\nContextWake installed successfully.\n\n'
    printf 'Next:\n  ctx doctor\n  ctx\n'
}

if [ "${CONTEXTWAKE_INSTALLER_SOURCE_ONLY:-0}" != "1" ]; then
    main "$@"
fi
