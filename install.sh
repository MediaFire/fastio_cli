#!/bin/sh
# Fast.io CLI installer
# Usage: curl -fsSL https://raw.githubusercontent.com/MediaFire/fastio_cli/main/install.sh | sh

set -e

REPO="MediaFire/fastio_cli"
INSTALL_DIR="${FASTIO_INSTALL_DIR:-/usr/local/bin}"

# Detect OS and architecture
detect_platform() {
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    ARCH=$(uname -m)

    case "$OS" in
        linux)  OS_NAME="linux" ;;
        darwin) OS_NAME="darwin" ;;
        *)      echo "Error: unsupported OS: $OS" >&2; exit 1 ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH_NAME="x64" ;;
        aarch64|arm64) ARCH_NAME="arm64" ;;
        *)             echo "Error: unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac

    BINARY="fastio-${OS_NAME}-${ARCH_NAME}"
    echo "$BINARY"
}

# Get the latest release tag from GitHub
get_latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | head -1 \
        | sed 's/.*"tag_name": *"//;s/".*//'
}

main() {
    echo "Fast.io CLI installer"
    echo ""

    BINARY=$(detect_platform)
    echo "Detected platform: $BINARY"

    VERSION=$(get_latest_version)
    if [ -z "$VERSION" ]; then
        echo "Error: could not determine latest version" >&2
        exit 1
    fi
    echo "Latest version: $VERSION"

    URL="https://github.com/${REPO}/releases/download/${VERSION}/${BINARY}"
    echo "Downloading from: $URL"
    echo ""

    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT

    if ! curl -fsSL -o "${TMP}/fastio" "$URL"; then
        echo "Error: download failed. Check that a release exists for your platform." >&2
        exit 1
    fi

    chmod +x "${TMP}/fastio"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${TMP}/fastio" "${INSTALL_DIR}/fastio"
    else
        echo "Installing to ${INSTALL_DIR} (requires sudo)..."
        sudo mv "${TMP}/fastio" "${INSTALL_DIR}/fastio"
    fi

    echo ""
    echo "Installed fastio to ${INSTALL_DIR}/fastio"
    echo ""
    "${INSTALL_DIR}/fastio" --version
    echo ""
    echo "Get started:"
    echo "  fastio auth login    # authenticate via browser"
    echo "  fastio --help        # see all commands"
}

main
