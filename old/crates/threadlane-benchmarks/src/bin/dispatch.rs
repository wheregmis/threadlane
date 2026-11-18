use std::sync::Arc;
use threadlane_protocol::{
    AgentToolDefinition, ImageAttachment, RuntimeToolCall, RuntimeToolCallFunction, ToolExecutor,
    ToolOutput,
};
use threadlane_provider::router::ProviderClient;
use threadlane_runtime::{AgentConfig, AgentRuntime};

const SAMPLES: usize = 10;
const BATCHES: usize = 20;

struct LargeOutputExecutor {
    output: ToolOutput,
}

#[async_trait::async_trait]
impl ToolExecutor for LargeOutputExecutor {
    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition::new(
            "large_output_probe",
            "",
            serde_json::json!({"type":"object"}),
        )]
        .into()
    }

    async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
        unreachable!("benchmark uses the rich output path")
    }

    async fn execute_tool_with_output_in_workspace(
        &self,
        _: &str,
        _: &str,
        _: Option<&std::path::Path>,
    ) -> Option<Result<ToolOutput, String>> {
        Some(Ok(self.output.clone()))
    }
}

#[hotpath::measure]
fn parallel_fresh_large_outputs(
    reactor: &tokio::runtime::Runtime,
    agent: &AgentRuntime,
    calls: &[RuntimeToolCall],
) {
    for _ in 0..4 {
        std::hint::black_box(reactor.block_on(agent.execute_tools(calls)).unwrap());
    }
}

fn call(index: usize, name: &str, arguments: serde_json::Value) -> RuntimeToolCall {
    RuntimeToolCall {
        id: format!("call-{index}"),
        r#type: "function".into(),
        function: RuntimeToolCallFunction {
            name: name.into(),
            arguments: arguments.to_string(),
        },
        thought_signature: None,
    }
}

#[hotpath::measure]
fn parallel_cached_searches(
    reactor: &tokio::runtime::Runtime,
    agent: &AgentRuntime,
    calls: &[RuntimeToolCall],
) {
    for _ in 0..BATCHES {
        std::hint::black_box(reactor.block_on(agent.execute_tools(calls)).unwrap());
    }
}

#[hotpath::measure]
fn parallel_cached_file_reads(
    reactor: &tokio::runtime::Runtime,
    agent: &AgentRuntime,
    calls: &[RuntimeToolCall],
) {
    for _ in 0..BATCHES {
        std::hint::black_box(reactor.block_on(agent.execute_tools(calls)).unwrap());
    }
}

#[hotpath::main(percentiles = [50, 95])]
fn main() {
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let content = "needle-0 needle-1 needle-2 needle-3 needle-4 needle-5 needle-6 needle-7\n";
    for index in 0..1_000 {
        std::fs::write(
            workspace.path().join(format!("file-{index:04}.txt")),
            content,
        )
        .unwrap();
    }
    let reactor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    // Only execute_tools runs: the provider client is inert and sends no requests.
    let mut agent = AgentRuntime::new_with_provider(
        "",
        None,
        "benchmark",
        Some(&journal.path().join("session.jsonl")),
        AgentConfig::default(),
        Arc::new(ProviderClient::new("", None)),
    )
    .unwrap();
    agent.work_dir = Some(workspace.path().to_path_buf());
    let searches: Vec<_> = (0..8)
        .map(|index| {
            call(
                index,
                "grep_search",
                serde_json::json!({"pattern": format!("needle-{index}")}),
            )
        })
        .collect();
    let reads: Vec<_> = (0..8)
        .map(|index| {
            call(
                index,
                "read_file",
                serde_json::json!({"path": format!("file-{index:04}.txt")}),
            )
        })
        .collect();
    for calls in [&searches, &reads] {
        reactor.block_on(agent.execute_tools(calls)).unwrap();
        let cached = reactor.block_on(agent.execute_tools(calls)).unwrap();
        assert!(cached
            .iter()
            .all(|result| !result.is_error && result.content.contains("served from cache")));
    }
    for _ in 0..SAMPLES {
        parallel_cached_searches(&reactor, &agent, &searches);
        parallel_cached_file_reads(&reactor, &agent, &reads);
    }
    // Keep this non-cacheable fixture after the cached suites: each execution
    // invalidates the cache, as a real extension/computer reply does.
    agent
        .register_tool_executor(Arc::new(LargeOutputExecutor {
            output: ToolOutput {
                content: "x".repeat(64 * 1024),
                images: vec![ImageAttachment {
                    display_name: "fixture.png".into(),
                    data_url: format!("data:image/png;base64,{}", "A".repeat(2 * 1024 * 1024)),
                }],
            },
        }))
        .unwrap();
    let large_outputs: Vec<_> = (0..8)
        .map(|index| call(index, "large_output_probe", serde_json::json!({})))
        .collect();
    let outputs = reactor
        .block_on(agent.execute_tools(&large_outputs))
        .unwrap();
    assert!(outputs.iter().all(|output| !output.is_error
        && output.content.len() == 64 * 1024
        && output.images.len() == 1
        && output.images[0].data_url.len() == 2 * 1024 * 1024 + "data:image/png;base64,".len()));
    for _ in 0..SAMPLES {
        parallel_fresh_large_outputs(&reactor, &agent, &large_outputs);
    }
}
