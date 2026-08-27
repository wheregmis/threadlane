hawkcheck:
    cargo +1.98.0 hawk check

hawkfix:
    cargo +1.98.0 hawk check --fix --allow-dirty

# Run the desktop app (macOS needs an app bundle; see scripts/run-gpui-macos.sh)
run *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" == "Darwin" ]]; then
        ./scripts/run-gpui-macos.sh {{ARGS}}
    else
        cargo run -p threadlane-gpui {{ARGS}}
    fi
