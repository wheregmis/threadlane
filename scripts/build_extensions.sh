#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MYPI_EXT_DIR="$ROOT_DIR/.mypi/extensions"
MYPI_AGENT_DIR="$ROOT_DIR/.mypi/agents"
MYPI_PROMPT_DIR="$ROOT_DIR/.mypi/prompts"

mkdir -p "$MYPI_EXT_DIR" "$MYPI_AGENT_DIR" "$MYPI_PROMPT_DIR"

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
for ext in "$ROOT_DIR"/extensions/*; do
    if [ -f "$ext/Cargo.toml" ]; then
        ext_name="$(basename "$ext")"
        wasm_path="$ROOT_DIR/target/wasm32-wasip1/release/${ext_name}.wasm"
        if [ ! -f "$wasm_path" ]; then
            echo "Missing compiled WASI module: $wasm_path" >&2
            exit 1
        fi
        # broker_smoke_ext is deployed as an ordinary extension module too.
        cp "$wasm_path" "$MYPI_EXT_DIR/${ext_name}.wasm"
    fi
done

# Install bundled agent presets and workflow prompts. Copy failures are fatal:
# a subagent tool without discoverable agent definitions is not a valid deployment.
for ext in "$ROOT_DIR/extensions"/*; do
    if [ -d "$ext/agents" ]; then
        cp -R "$ext/agents/." "$MYPI_AGENT_DIR/"
    fi
    if [ -d "$ext/prompts" ]; then
        cp -R "$ext/prompts/." "$MYPI_PROMPT_DIR/"
    fi
done

echo "Successfully deployed WASI binaries, agents, and prompts under $ROOT_DIR/.mypi!"
