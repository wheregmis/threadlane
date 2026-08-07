use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct ExecutionContext {
    pub session_id: Option<String>,
    pub lane: Option<String>,
    pub run_id: Option<String>,
    attributes: BTreeMap<String, String>,
}

impl ExecutionContext {
    pub fn with_run(
        session_id: impl Into<String>,
        lane: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            lane: Some(lane.into()),
            run_id: Some(run_id.into()),
            attributes: BTreeMap::new(),
        }
    }

    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let sensitive = [
            "prompt",
            "completion",
            "argument",
            "output",
            "header",
            "credential",
            "content",
            "response",
        ];
        if sensitive
            .iter()
            .any(|part| key.to_ascii_lowercase().contains(part))
        {
            return;
        }
        self.attributes.insert(key, value.into());
    }

    pub fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }
}

pub trait TelemetrySink: Send + Sync {
    fn event(&self, _name: &str, _context: &ExecutionContext) {}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTelemetry;

impl TelemetrySink for NoopTelemetry {}
