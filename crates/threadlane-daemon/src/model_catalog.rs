use threadlane_protocol::capabilities::ModelDescriptor;

pub fn models() -> Vec<ModelDescriptor> {
    [
        ("gpt-5.6-luna", "GPT-5.6 Luna", "openai", true, 128_000),
        ("gpt-5.4", "GPT-5.4", "openai", true, 128_000),
        ("gpt-5.4-mini", "GPT-5.4 Mini", "openai", true, 128_000),
        ("gpt-5.5", "GPT-5.5", "openai", true, 128_000),
        ("gpt-5.6-sol", "GPT-5.6 Sol", "openai", true, 128_000),
        ("gpt-5.6-terra", "GPT-5.6 Terra", "openai", true, 128_000),
        (
            "gpt-5.3-codex-spark",
            "GPT-5.3 Codex Spark",
            "openai",
            true,
            128_000,
        ),
        ("gpt-4o", "GPT-4o", "openai", false, 128_000),
        ("gpt-4o-mini", "GPT-4o Mini", "openai", false, 128_000),
        (
            "antigravity/gemini-3.7-flash",
            "Gemini 3.7 Flash",
            "antigravity",
            true,
            1_000_000,
        ),
        (
            "antigravity/gemini-3.1-pro",
            "Gemini 3.1 Pro",
            "antigravity",
            true,
            2_000_000,
        ),
        (
            "antigravity/claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            "antigravity",
            true,
            128_000,
        ),
        (
            "antigravity/claude-opus-4-6",
            "Claude Opus 4.6",
            "antigravity",
            true,
            128_000,
        ),
        (
            "antigravity/gpt-oss-120b",
            "GPT-OSS 120B",
            "antigravity",
            true,
            128_000,
        ),
        (
            "opencode-go/mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/mimo-v2.5",
            "MiMo V2.5",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/qwen3.8-max",
            "Qwen 3.8 Max",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/minimax-m3",
            "MiniMax M3",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/minimax-m2.7",
            "MiniMax M2.7",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/deepseek-v4-pro",
            "DeepSeek V4 Pro",
            "opencode-go",
            true,
            128_000,
        ),
        (
            "opencode-go/deepseek-v4-flash",
            "DeepSeek V4 Flash",
            "opencode-go",
            true,
            128_000,
        ),
        ("opencode-go/hy3", "HY 3", "opencode-go", true, 128_000),
    ]
    .into_iter()
    .map(
        |(id, name, provider, supports_reasoning, context_window)| ModelDescriptor {
            id: id.into(),
            name: name.into(),
            provider: provider.into(),
            supports_reasoning,
            context_window: Some(context_window),
        },
    )
    .collect()
}
