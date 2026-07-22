#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MYPI_EXT_DIR="$ROOT_DIR/.mypi/extensions"

mkdir -p "$MYPI_EXT_DIR"

echo "Building WASI extensions (--target wasm32-wasip1 --release)..."
for ext in "$ROOT_DIR/extensions"/*; do
    if [ -f "$ext/Cargo.toml" ]; then
        echo "  Compiling WASI extension: $(basename "$ext")..."
        cargo build --manifest-path "$ext/Cargo.toml" --target wasm32-wasip1 --release
    fi
done

echo "Cleaning previous runtime binaries in $MYPI_EXT_DIR..."
rm -rf "$MYPI_EXT_DIR"/*

echo "Deploying compiled .wasm binaries to $MYPI_EXT_DIR..."
find "$ROOT_DIR/target/wasm32-wasip1/release" -maxdepth 1 -name "*.wasm" -exec cp {} "$MYPI_EXT_DIR/" \; 2>/dev/null || true

# Copy agent presets and workflow prompts to .mypi/agents and .mypi/prompts
for ext in "$ROOT_DIR/extensions"/*; do
    if [ -d "$ext" ]; then
        if [ -d "$ext/agents" ]; then
            mkdir -p "$ROOT_DIR/.mypi/agents"
            cp -r "$ext/agents/"* "$ROOT_DIR/.mypi/agents/" 2>/dev/null || true
        fi
        if [ -d "$ext/prompts" ]; then
            mkdir -p "$ROOT_DIR/.mypi/prompts"
            cp -r "$ext/prompts/"* "$ROOT_DIR/.mypi/prompts/" 2>/dev/null || true
        fi
    fi
done

echo "Successfully deployed WASI binaries to $MYPI_EXT_DIR!"
