#!/bin/bash

set -e

BINARY_URL="https://github.com/ChasmHQ/chasm/releases/download/pre-release/chasm"
INSTALL_DIR="/usr/local/bin"
BINARY_NAME="chasm"

echo "Chasm Installer"
echo "==============="
echo ""

# Check if running on macOS
if [[ "$OSTYPE" != "darwin"* ]]; then
    echo "Error: This installer is for macOS only"
    exit 1
fi

# Check if Foundry is installed
if ! command -v forge &> /dev/null; then
    echo "Warning: Foundry is not installed. Chasm requires Foundry (anvil, cast, forge)."
    echo "Install Foundry from: https://getfoundry.sh"
    echo ""
    read -p "Continue installation anyway? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Create temporary directory
TMP_DIR=$(mktemp -d)
TMP_FILE="$TMP_DIR/$BINARY_NAME"

echo "Downloading Chasm binary..."
if ! curl -L --progress-bar "$BINARY_URL" -o "$TMP_FILE"; then
    echo "Error: Failed to download binary"
    rm -rf "$TMP_DIR"
    exit 1
fi

echo "Making binary executable..."
chmod +x "$TMP_FILE"

# Check if we need sudo for installation
if [ -w "$INSTALL_DIR" ]; then
    echo "Installing to $INSTALL_DIR/$BINARY_NAME..."
    mv "$TMP_FILE" "$INSTALL_DIR/$BINARY_NAME"
else
    echo "Installing to $INSTALL_DIR/$BINARY_NAME (requires sudo)..."
    sudo mv "$TMP_FILE" "$INSTALL_DIR/$BINARY_NAME"
fi

# Cleanup
rm -rf "$TMP_DIR"

# Verify installation
if command -v chasm &> /dev/null; then
    echo ""
    echo "✓ Chasm installed successfully!"
    echo ""
    echo "Usage:"
    echo "  chasm .              # Run in current directory"
    echo "  chasm ./contracts    # Run in specific directory"
    echo ""
    echo "The web UI will be available at http://localhost:3000"
else
    echo ""
    echo "Error: Installation completed but 'chasm' command not found in PATH"
    echo "You may need to add $INSTALL_DIR to your PATH or restart your terminal"
    exit 1
fi
