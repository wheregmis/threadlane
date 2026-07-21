use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct WasiCommandDefinition {
    name: String,
    description: String,
}

#[derive(Serialize, Deserialize)]
struct WasiExtensionManifest {
    api_version: u32,
    name: String,
    version: String,
    description: String,
    commands: Vec<WasiCommandDefinition>,
    hooks: Vec<String>,
}

#[derive(Deserialize)]
struct Invocation {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
    #[serde(default)]
    state: PlanState,
}

#[derive(Default, Deserialize, Serialize)]
struct PlanState {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    items: Vec<PlanItem>,
}

#[derive(Deserialize, Serialize)]
struct PlanItem {
    index: usize,
    description: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Effect {
    SetToolPolicy { policy: String },
    RequestModelTurn { prompt: String },
}

#[derive(Serialize)]
struct Response {
    message: String,
    state: PlanState,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    effects: Vec<Effect>,
}

static mut OUTPUT_BUF: Vec<u8> = Vec::new();

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buf = vec![0u8; size as usize];
    let ptr = buf.as_mut_ptr() as i32;
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn extension_info() -> u64 {
    write_output(&WasiExtensionManifest {
        api_version: 1,
        name: "plan_mode_ext".into(),
        version: "0.3.0".into(),
        description: "Reference stateful plan-mode extension with lifecycle hooks".into(),
        commands: vec![
            WasiCommandDefinition {
                name: "plan".into(),
                description: "Enable read-only planning and request a plan".into(),
            },
            WasiCommandDefinition {
                name: "todos".into(),
                description: "Display extension-owned plan items".into(),
            },
        ],
        hooks: vec!["assistant_message".into()],
    })
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let mut state = invocation.state;
    let mut effects = Vec::new();
    let message = match invocation.name.as_str() {
        "plan" => {
            state.enabled = !state.enabled;
            if state.enabled {
                state.items.clear();
                effects.push(Effect::SetToolPolicy {
                    policy: "read_only".into(),
                });
                if let Some(task) = invocation
                    .arguments
                    .get("raw")
                    .and_then(|value| value.as_str())
                {
                    if !task.trim().is_empty() {
                        effects.push(Effect::RequestModelTurn {
                            prompt: format!(
                                "Analyze the workspace in read-only mode and propose a concise numbered implementation plan for: {task}"
                            ),
                        });
                    }
                }
                "🟢 WASI Plan Mode ENABLED (read-only policy active)".to_string()
            } else {
                effects.push(Effect::SetToolPolicy {
                    policy: "full".into(),
                });
                "⚪ WASI Plan Mode DISABLED".to_string()
            }
        }
        "todos" if state.enabled => format_todos(&state.items),
        "todos" => "📋 Plan Mode is disabled. Toggle on using /plan.".to_string(),
        other => format!("Unknown WASI plan command: {other}"),
    };

    write_output(&Response {
        message,
        state,
        effects,
    })
}

#[no_mangle]
pub extern "C" fn handle_hook(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let mut state = invocation.state;
    let message = match invocation.name.as_str() {
        "assistant_message" if state.enabled => {
            if let Some(content) = invocation
                .arguments
                .get("content")
                .and_then(|value| value.as_str())
            {
                let items = parse_plan_items(content);
                if !items.is_empty() {
                    state.items = items;
                    "Plan items updated.".to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };
    write_output(&Response {
        message,
        state,
        effects: vec![],
    })
}

fn parse_invocation(ptr: i32, len: i32) -> Invocation {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(input).unwrap_or(Invocation {
        name: String::new(),
        arguments: serde_json::Value::Null,
        state: PlanState::default(),
    })
}

fn parse_plan_items(text: &str) -> Vec<PlanItem> {
    let mut in_plan = false;
    let mut items = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let heading = trimmed
            .trim_start_matches('#')
            .trim()
            .trim_end_matches(':')
            .trim();
        if heading.eq_ignore_ascii_case("plan")
            || heading.eq_ignore_ascii_case("implementation plan")
        {
            in_plan = true;
            continue;
        }
        if !in_plan {
            continue;
        }
        if let Some((index, description)) = parse_ordered_item(trimmed) {
            items.push(PlanItem {
                index,
                description: description.into(),
            });
        } else if !trimmed.is_empty() && (trimmed.starts_with('#') || !trimmed.starts_with('-')) {
            break;
        }
    }
    items
}

fn parse_ordered_item(line: &str) -> Option<(usize, &str)> {
    let digits_end = line.find(|character: char| !character.is_ascii_digit())?;
    let index = line[..digits_end].parse::<usize>().ok()?;
    let delimiter = line[digits_end..].chars().next()?;
    if !(1..=50).contains(&index) || !matches!(delimiter, '.' | ')') {
        return None;
    }
    let description = line[digits_end + delimiter.len_utf8()..].trim();
    (!description.is_empty()).then_some((index, description))
}

fn format_todos(items: &[PlanItem]) -> String {
    if items.is_empty() {
        return "📋 No plan items yet. Waiting for the planning response.".to_string();
    }
    let mut output = String::from("📋 Current Plan:\n");
    for item in items {
        output.push_str(&format!("  ⏳ {}. {}\n", item.index, item.description));
    }
    output
}

fn write_output<T: Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).expect("extension response must serialize");
    let len = bytes.len() as u64;
    unsafe {
        OUTPUT_BUF = bytes;
        let ptr = OUTPUT_BUF.as_ptr() as u64;
        (ptr << 32) | (len & 0xFFFF_FFFF)
    }
}
