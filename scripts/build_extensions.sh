#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
THREADLANE_EXT_DIR="$ROOT_DIR/.threadlane/extensions"
THREADLANE_AGENT_DIR="$ROOT_DIR/.threadlane/agents"
THREADLANE_PROMPT_DIR="$ROOT_DIR/.threadlane/prompts"

mkdir -p "$THREADLANE_EXT_DIR" "$THREADLANE_AGENT_DIR" "$THREADLANE_PROMPT_DIR"

echo "Building WASI extensions (--target wasm32-wasip1 --release)..."
for ext in "$ROOT_DIR/extensions"/*; do
    if [ -f "$ext/Cargo.toml" ]; then
        echo "  Compiling WASI extension: $(basename "$ext")..."
        cargo build --manifest-path "$ext/Cargo.toml" --target wasm32-wasip1 --release
    fi
done

echo "Cleaning previous runtime binaries in $THREADLANE_EXT_DIR..."
rm -rf "$THREADLANE_EXT_DIR"/*

echo "Deploying compiled .wasm binaries to $THREADLANE_EXT_DIR..."
for ext in "$ROOT_DIR"/extensions/*; do
    if [ -f "$ext/Cargo.toml" ]; then
        ext_name="$(basename "$ext")"
        wasm_path="$ROOT_DIR/target/wasm32-wasip1/release/${ext_name}.wasm"
        if [ ! -f "$wasm_path" ]; then
            echo "Missing compiled WASI module: $wasm_path" >&2
            exit 1
        fi
        # broker_smoke_ext is deployed as an ordinary extension module too.
        cp "$wasm_path" "$THREADLANE_EXT_DIR/${ext_name}.wasm"
    fi
done

# Install bundled agent presets and workflow prompts. Copy failures are fatal:
# a subagent tool without discoverable agent definitions is not a valid deployment.
for ext in "$ROOT_DIR/extensions"/*; do
    if [ -d "$ext/agents" ]; then
        cp -R "$ext/agents/." "$THREADLANE_AGENT_DIR/"
    fi
    if [ -d "$ext/prompts" ]; then
        cp -R "$ext/prompts/." "$THREADLANE_PROMPT_DIR/"
    fi
done

echo "Successfully deployed WASI binaries, agents, and prompts under $ROOT_DIR/.threadlane!"
