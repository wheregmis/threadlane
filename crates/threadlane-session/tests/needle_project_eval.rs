#![cfg(feature = "needle")]

use std::process::Command;

#[test]
fn project_evaluator_does_not_require_a_tools_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_needle-project-eval"))
        .arg("--help")
        .output()
        .expect("project evaluator should run");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--project <directory>"));
    assert!(stdout.contains("--sessions <directory>"));
    assert!(!stdout.contains("--tools"));
}
