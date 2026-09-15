#!/bin/sh
set -eu

path_line='export PATH="$HOME/.local/bin:$PATH" # ContextWake installer'
install_directory=${CONTEXTWAKE_INSTALL_DIR:-"$HOME/.local/bin"}

case "$install_directory" in
    /*) ;;
    *)
        printf 'ContextWake uninstall failed: CONTEXTWAKE_INSTALL_DIR must be an absolute path\n' >&2
        exit 1
        ;;
esac

if [ -L "$install_directory" ]; then
    printf 'ContextWake uninstall failed: install directory must not be a symbolic link\n' >&2
    exit 1
fi

path_marker="$install_directory/.contextwake-path-added"
if [ -f "$path_marker" ] && [ ! -L "$path_marker" ]; then
    shell_profile=$(sed -n '1p' "$path_marker")
    case "$shell_profile" in
        "$HOME/.bashrc" | "$HOME/.zshrc" | "$HOME/.profile") ;;
        *)
            printf 'ContextWake uninstall failed: installer PATH marker is invalid\n' >&2
            exit 1
            ;;
    esac
    if [ -f "$shell_profile" ] && [ ! -L "$shell_profile" ]; then
        profile_temp=$(mktemp "${TMPDIR:-/tmp}/contextwake-profile.XXXXXX")
        awk -v owned="$path_line" '
            $0 == owned && !removed { removed = 1; next }
            { print }
        ' "$shell_profile" > "$profile_temp"
        cp "$profile_temp" "$shell_profile"
        rm -f "$profile_temp"
    fi
    rm -f "$path_marker"
fi

for binary_name in ctx ctxwake; do
    binary_path="$install_directory/$binary_name"
    if [ -d "$binary_path" ] && [ ! -L "$binary_path" ]; then
        printf 'ContextWake uninstall failed: refusing to remove a directory at %s\n' "$binary_path" >&2
        exit 1
    fi
    rm -f "$binary_path"
done

if [ -d "$install_directory" ]; then
    rmdir "$install_directory" 2>/dev/null || true
fi

printf 'ContextWake binaries were removed.\n'
printf 'Profiles, workspaces, sessions, checkpoints, handoffs, and configuration were preserved.\n'
