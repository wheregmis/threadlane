hawkcheck:
    cargo +1.98.0 hawk check

hawkfix:
    cargo +1.98.0 hawk check --fix --allow-dirty

evaluate_local project="." sessions=".threadlane/sessions":
    cargo run --release -p threadlane-session --features needle --bin needle-project-eval -- \
        --project "{{project}}" \
        --sessions "{{sessions}}"
