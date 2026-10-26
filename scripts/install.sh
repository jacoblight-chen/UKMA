#!/bin/sh
# Install (or remove) the UKMA CLI. No Node.js, no compiler.
#
#   curl -fsSL https://raw.githubusercontent.com/jacoblight-chen/UKMA/main/scripts/install.sh | sh
#
# Offline / air-gapped: build locally and install the binary you built.
#
#   cargo build -p ukma-cli --release
#   ./scripts/install.sh --from-file target/release/ukma
#
# After installing, run `ukma install` in a new terminal to wire your coding
# agents, then `ukma init` inside each project.
set -eu

VERSION=${UKMA_VERSION:-latest}
BASE_URL=${UKMA_BASE_URL:-https://github.com/jacoblight-chen/UKMA/releases}
INSTALL_DIR=${UKMA_INSTALL_DIR:-}
FROM_FILE=""
NO_PATH=0
FORCE=0
UNINSTALL=0

usage() {
    cat <<'EOF'
usage: install.sh [options]

  --version <v>      release to install (default: latest)
  --dir <path>       install directory (default: ~/.local/bin)
  --from-file <path> install this local binary instead of downloading
  --base-url <url>   release base URL
  --no-path          do not modify a shell rc file
  --force            replace an existing binary
  --uninstall        unwire the agents and delete the binary
  -h, --help         this text
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION=$2; shift 2 ;;
        --dir) INSTALL_DIR=$2; shift 2 ;;
        --from-file) FROM_FILE=$2; shift 2 ;;
        --base-url) BASE_URL=$2; shift 2 ;;
        --no-path) NO_PATH=1; shift ;;
        --force) FORCE=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$INSTALL_DIR" ]; then
    INSTALL_DIR="$HOME/.local/bin"
fi

detect_triple() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) os_part=unknown-linux-gnu ;;
        Darwin) os_part=apple-darwin ;;
        *) echo "unsupported OS: $os (use --from-file)" >&2; exit 1 ;;
    esac
    case "$arch" in
        x86_64|amd64) arch_part=x86_64 ;;
        arm64|aarch64) arch_part=aarch64 ;;
        *) echo "unsupported architecture: $arch (use --from-file)" >&2; exit 1 ;;
    esac
    printf '%s-%s' "$arch_part" "$os_part"
}

download() {
    url=$1
    dest=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$dest" "$url"
    else
        echo "curl or wget is required to download a release (use --from-file)" >&2
        return 1
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo ""
    fi
}

extract_zip() {
    archive=$1
    dest=$2
    if command -v unzip >/dev/null 2>&1; then
        unzip -o -q "$archive" -d "$dest"
    elif command -v bsdtar >/dev/null 2>&1; then
        mkdir -p "$dest"
        bsdtar -xf "$archive" -C "$dest"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$archive" "$dest"
    else
        echo "unzip, bsdtar or python3 is required to unpack the release (use --from-file)" >&2
        return 1
    fi
}

add_path() {
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) return 0 ;;
    esac
    line="export PATH=\"$INSTALL_DIR:\$PATH\""
    for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
        if [ -f "$rc" ]; then
            if ! grep -Fq "$INSTALL_DIR" "$rc" 2>/dev/null; then
                printf '\n# ukma\n%s\n' "$line" >> "$rc"
            fi
            echo "Added $INSTALL_DIR to $rc (open a new terminal to pick it up)"
            return 0
        fi
    done
    printf '\n# ukma\n%s\n' "$line" >> "$HOME/.profile"
    echo "Added $INSTALL_DIR to $HOME/.profile (open a new terminal to pick it up)"
}

if [ "$UNINSTALL" -eq 1 ]; then
    echo "Removing the UKMA CLI from $INSTALL_DIR"
    if command -v ukma >/dev/null 2>&1; then
        echo "  unwiring the agents (ukma uninstall --yes)"
        ukma uninstall --yes || true
    fi
    rm -f "$INSTALL_DIR/ukma"
    echo "  deleted $INSTALL_DIR/ukma"
    # `ukma uninstall` vacates a running image by renaming it; sweep that too.
    rm -f "$INSTALL_DIR/ukma.old"
    echo "Remove the '# ukma' block from your shell rc if you added one."
    echo "Project directories (.ukma) were left alone; remove them with \`ukma uninit\` per project."
    exit 0
fi

mkdir -p "$INSTALL_DIR"

if [ -n "$FROM_FILE" ]; then
    if [ ! -f "$FROM_FILE" ]; then
        echo "not found: $FROM_FILE" >&2
        exit 1
    fi
    cp "$FROM_FILE" "$INSTALL_DIR/ukma"
    echo "Installed ukma from $FROM_FILE"
elif [ -x "$INSTALL_DIR/ukma" ] && [ "$FORCE" -eq 0 ]; then
    echo "ukma is already installed: $("$INSTALL_DIR/ukma" --version) ($INSTALL_DIR/ukma)"
    echo "Re-run with --force to replace it, or run \`ukma upgrade\`."
    exit 0
else
    triple=$(detect_triple)
    asset="ukma-$triple.zip"
    if [ "$VERSION" = "latest" ]; then
        url="$BASE_URL/latest/download/$asset"
    else
        pinned=$(printf '%s' "$VERSION" | sed 's/^v//')
        url="$BASE_URL/download/v$pinned/$asset"
    fi
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM
    echo "Downloading $url"
    if ! download "$url" "$tmp/$asset"; then
        echo "download failed; install a local build instead: $0 --from-file target/release/ukma" >&2
        exit 1
    fi
    if download "$url.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
        expected=$(cut -d' ' -f1 "$tmp/$asset.sha256")
        actual=$(sha256_of "$tmp/$asset")
        if [ -n "$actual" ] && [ "$expected" != "$actual" ]; then
            echo "SHA-256 mismatch: expected $expected, got $actual" >&2
            exit 1
        fi
        if [ -n "$actual" ]; then
            echo "  SHA-256 verified"
        fi
    fi
    if ! extract_zip "$tmp/$asset" "$tmp/out"; then
        echo "could not unpack $asset; install a local build instead: $0 --from-file target/release/ukma" >&2
        exit 1
    fi
    found=$(find "$tmp/out" -type f -name ukma | head -n 1)
    if [ -z "$found" ]; then
        echo "the archive $asset does not contain a ukma binary" >&2
        exit 1
    fi
    cp "$found" "$INSTALL_DIR/ukma"
    echo "Installed ukma to $INSTALL_DIR/ukma"
fi

chmod +x "$INSTALL_DIR/ukma"

if [ "$NO_PATH" -eq 0 ]; then
    add_path
fi

version=$("$INSTALL_DIR/ukma" --version 2>/dev/null || echo 'ukma')
echo ""
echo "$version is installed."
echo "Next steps:"
echo "  1. open a new terminal"
echo "  2. ukma install      # wire Claude Code, Cursor, Codex, Copilot, ..."
echo "  3. ukma init         # inside each project"
