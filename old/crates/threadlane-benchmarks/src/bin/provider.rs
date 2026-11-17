use threadlane_protocol::{AgentMessage, ImageAttachment, RuntimeToolCall, RuntimeToolCallFunction};
use threadlane_provider::convert::{convert_to_codex_llm, convert_to_llm};

const SAMPLES: usize = 10;
const CONVERSIONS: usize = 20;

fn history(with_images: bool) -> Vec<AgentMessage> {
    let mut messages = vec![AgentMessage::System {
        content: "system prompt".repeat(400),
    }];
    if with_images {
        messages.push(AgentMessage::user(
            "Inspect this image",
            vec![ImageAttachment {
                display_name: "input.jpg".into(),
                data_url: format!("data:image/jpeg;base64,{}", "A".repeat(1024 * 1024)),
            }],
        ));
    }
    for index in 0..60 {
        let id = format!("call_{index}");
        messages.push(AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![RuntimeToolCall {
                id: id.clone(),
                r#type: "function".into(),
                function: RuntimeToolCallFunction {
                    name: "read_file".into(),
                    arguments: format!(r#"{{"path":"file_{index}.rs"}}"#),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        });
        messages.push(AgentMessage::Tool {
            tool_call_id: id,
            name: "read_file".into(),
            content: "source line\n".repeat(667),
            is_error: false,
            terminate: false,
            images: if with_images && index >= 58 {
                vec![ImageAttachment {
                    display_name: "screenshot.jpg".into(),
                    data_url: format!("data:image/jpeg;base64,{}", "A".repeat(1024 * 1024)),
                }]
            } else {
                vec![]
            },
        });
    }
    messages
}

#[hotpath::measure]
fn chat_text(messages: &[AgentMessage]) {
    for _ in 0..CONVERSIONS {
        std::hint::black_box(convert_to_llm(messages));
    }
}

#[hotpath::measure]
fn responses_text(messages: &[AgentMessage]) {
    for _ in 0..CONVERSIONS {
        std::hint::black_box(convert_to_codex_llm(messages));
    }
}

#[hotpath::measure]
fn chat_images(messages: &[AgentMessage]) {
    for _ in 0..CONVERSIONS {
        std::hint::black_box(convert_to_llm(messages));
    }
}

#[hotpath::measure]
fn responses_images(messages: &[AgentMessage]) {
    for _ in 0..CONVERSIONS {
        std::hint::black_box(convert_to_codex_llm(messages));
    }
}

#[hotpath::main(percentiles = [50, 95])]
fn main() {
    let text = history(false);
    let images = history(true);
    // Warm conversion and allocator paths before collecting samples.
    for _ in 0..10 {
        for messages in [&text, &images] {
            std::hint::black_box(convert_to_llm(messages));
            std::hint::black_box(convert_to_codex_llm(messages));
        }
    }
    for _ in 0..SAMPLES {
        chat_text(&text);
        responses_text(&text);
        chat_images(&images);
        responses_images(&images);
    }
}
