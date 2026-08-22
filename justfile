hawkcheck:
    cargo +1.98.0 hawk check

hawkfix:
    cargo +1.98.0 hawk check --fix --allow-dirty

evaluate_local sessions=".threadlane/sessions" tools=".threadlane/provider-tools.json":
    cargo run -p threadlane-runtime --features needle --bin needle-history-eval -- \
        --sessions "{{sessions}}" \
        --tools "{{tools}}"
