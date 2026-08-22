hawkcheck:
    cargo +1.98.0 hawk check

hawkfix:
    cargo +1.98.0 hawk check --fix --allow-dirty

evaluate_local project="." sessions=".threadlane/sessions":
    cargo run --release -p threadlane-session --features needle --bin needle-project-eval -- \
        --project "{{project}}" \
        --sessions "{{sessions}}"

needle_dataset project="." sessions=".threadlane/sessions" work_dir=".threadlane/needle-training" replace="":
    cargo run --release -p threadlane-session --features needle --bin needle-project-train -- \
        dataset \
        --project "{{project}}" \
        --sessions "{{sessions}}" \
        --work-dir "{{work_dir}}" \
        {{replace}}
