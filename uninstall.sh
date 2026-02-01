#!/bin/bash

set -e

INSTALL_DIR="/usr/local/bin"
BINARY_NAME="chasm"
KEYSTORE_DIR="$HOME/.chasm"

echo "Chasm Uninstaller"
echo "================="
echo ""

# Check if binary exists
if [ ! -f "$INSTALL_DIR/$BINARY_NAME" ]; then
    echo "Chasm is not installed at $INSTALL_DIR/$BINARY_NAME"
    exit 1
fi

# Remove binary
if [ -w "$INSTALL_DIR" ]; then
    echo "Removing $INSTALL_DIR/$BINARY_NAME..."
    rm -f "$INSTALL_DIR/$BINARY_NAME"
else
    echo "Removing $INSTALL_DIR/$BINARY_NAME (requires sudo)..."
    sudo rm -f "$INSTALL_DIR/$BINARY_NAME"
fi

# Ask about keystore removal
if [ -d "$KEYSTORE_DIR" ]; then
    echo ""
    read -p "Remove keystore directory ($KEYSTORE_DIR)? This will delete all saved wallets. (y/N) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "Removing keystore directory..."
        rm -rf "$KEYSTORE_DIR"
    else
        echo "Keystore directory preserved"
    fi
fi

echo ""
echo "✓ Chasm uninstalled successfully"
