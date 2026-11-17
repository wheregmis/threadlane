use serde_json::json;
use threadlane_provider::antigravity::build_gemini_request;

#[test]
fn test_gemini_request_building() {
    let system_prompt = "You are a helpful AI coding assistant.";
    let messages = vec![
        json!({
            "role": "user",
            "content": "Write a hello world function in Rust."
        }),
        json!({
            "role": "assistant",
            "content": "Here is a simple Hello World in Rust:\n```rust\nfn main() {\n    println!(\"Hello, world!\");\n}\n```"
        }),
    ];
    let tools = vec![json!({
        "name": "run_command",
        "description": "Execute a shell command",
        "parameters": {
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }
    })];

    let req = build_gemini_request(system_prompt, &messages, &tools);

    assert!(
        req.system_instruction.is_some(),
        "System instruction should be set"
    );
    assert_eq!(req.contents.len(), 2, "Should contain 2 messages");
    assert_eq!(req.contents[0].role, "user");
    assert_eq!(req.contents[1].role, "model");

    assert!(req.tools.is_some(), "Tools should be present");
    let decls = &req.tools.unwrap()[0].function_declarations;
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].name, "run_command");
}
