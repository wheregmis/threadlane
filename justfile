# bindgen (via gpui-pre-media) needs a working libclang: Homebrew LLVM's
# libclang is built against a newer LLVM than the pinned toolchain ships,
# so point at Xcode's libclang on macOS.
hawkcheck:
    LIBCLANG_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib cargo +1.98.0 hawk check

hawkfix:
    LIBCLANG_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib cargo +1.98.0 hawk check --fix --allow-dirty

# Run the desktop app (macOS needs an app bundle; see scripts/run-gpui-macos.sh)
run *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" == "Darwin" ]]; then
        ./scripts/run-gpui-macos.sh {{ARGS}}
    else
        cargo run -p threadlane-gpui {{ARGS}}
    fi
