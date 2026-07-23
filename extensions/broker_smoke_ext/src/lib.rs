use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct WasiCommandDefinition {
    name: String,
    description: String,
}

#[derive(Serialize)]
struct WasiExtensionManifest {
    api_version: u32,
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    commands: Vec<WasiCommandDefinition>,
}

#[derive(Deserialize)]
struct Invocation {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
    #[serde(default)]
    events: Vec<ExtensionEvent>,
}

#[derive(Deserialize)]
struct ExtensionEvent {
    topic: String,
}

#[derive(Serialize)]
struct BrokerRequest {
    api_version: u32,
    capability: String,
    operation: String,
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct BrokerResponse {
    ok: bool,
    error: Option<BrokerError>,
}

#[derive(Deserialize)]
struct BrokerError {
    code: String,
}

#[derive(Serialize)]
struct Response {
    message: String,
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "threadlane_host")]
extern "C" {
    #[link_name = "request"]
    fn broker_request(
        request_ptr: i32,
        request_len: i32,
        response_ptr: i32,
        response_capacity: i32,
    ) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn broker_request(
    _request_ptr: i32,
    _request_len: i32,
    _response_ptr: i32,
    _response_capacity: i32,
) -> i32 {
    -1
}

static mut OUTPUT_PTR: *mut u8 = std::ptr::null_mut();
static mut OUTPUT_LEN: usize = 0;
static mut OUTPUT_CAPACITY: usize = 0;

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
        api_version: 2,
        name: "broker_smoke_ext".into(),
        version: "0.1.0".into(),
        description: "Capability broker ABI smoke test".into(),
        capabilities: vec![if cfg!(feature = "agent-only") {
            "agent"
        } else {
            "tools"
        }
        .into()],
        commands: vec![WasiCommandDefinition {
            name: "broker-smoke".into(),
            description: "Broker ABI smoke test".into(),
        }],
    })
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    let invocation = parse_invocation(ptr, len);
    let message = if invocation.name == "broker-smoke" {
        if invocation
            .events
            .iter()
            .any(|event| event.topic == "broker_response")
        {
            "received broker_response event".into()
        } else {
            request_broker(
                invocation
                    .arguments
                    .get("mode")
                    .and_then(|mode| mode.as_str()),
            )
        }
    } else {
        "unknown command".into()
    };
    write_output(&Response { message })
}

fn request_broker(mode: Option<&str>) -> String {
    let request = if mode == Some("malformed") {
        b"{".to_vec()
    } else {
        serde_json::to_vec(&BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: if mode == Some("result-event") {
                "get_policy"
            } else {
                "set_policy"
            }
            .into(),
            arguments: if mode == Some("result-event") {
                serde_json::Value::Null
            } else {
                serde_json::json!({"policy": "read_only"})
            },
        })
        .expect("broker request serializes")
    };
    let mut response = vec![
        0xa5;
        if mode == Some("small-output") {
            1
        } else {
            1024
        }
    ];
    let written = unsafe {
        broker_request(
            request.as_ptr() as i32,
            if mode == Some("huge-length") {
                i32::MAX
            } else {
                request.len() as i32
            },
            response.as_mut_ptr() as i32,
            response.len() as i32,
        )
    };
    if written < 0 {
        return if mode == Some("huge-length") && written == -1 {
            "broker invalid range".into()
        } else if response.iter().all(|byte| *byte == 0xa5) {
            "broker response too large".into()
        } else {
            "broker response modified".into()
        };
    }
    if written > 0 && (written as usize) <= response.len() {
        if let Ok(response) =
            serde_json::from_slice::<BrokerResponse>(&response[..written as usize])
        {
            if response.ok {
                return "broker accepted tools.set_policy".into();
            }
            if let Some(error) = response.error {
                return format!("broker denied {}", error.code);
            }
        }
    }
    "broker request failed".into()
}

fn parse_invocation(ptr: i32, len: i32) -> Invocation {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(input).unwrap_or(Invocation {
        name: String::new(),
        arguments: serde_json::Value::Null,
        events: Vec::new(),
    })
}

fn write_output<T: Serialize>(value: &T) -> u64 {
    let mut bytes = serde_json::to_vec(value).expect("extension response serializes");
    let len = bytes.len();
    let capacity = bytes.capacity();
    let ptr = bytes.as_mut_ptr();
    unsafe {
        if !OUTPUT_PTR.is_null() {
            drop(Vec::from_raw_parts(OUTPUT_PTR, OUTPUT_LEN, OUTPUT_CAPACITY));
        }
        OUTPUT_PTR = ptr;
        OUTPUT_LEN = len;
        OUTPUT_CAPACITY = capacity;
    }
    std::mem::forget(bytes);
    ((ptr as u64) << 32) | (len as u64 & 0xFFFF_FFFF)
}
