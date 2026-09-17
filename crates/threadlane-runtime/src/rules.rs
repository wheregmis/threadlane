use crate::config::AgentConfig;
use crate::harness::ToolReplaySafety;
use serde::{Deserialize, Serialize};

pub fn classify_tool_replay_safety(tool_name: &str) -> ToolReplaySafety {
    match tool_name {
        "view_file" | "read_file" | "list_dir" | "grep_search" | "list_permissions" => {
            ToolReplaySafety::Safe
        }
        _ => ToolReplaySafety::Never,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRule {
    id: String,
    name: String,
    pub(crate) pattern: String,
    reminder: String,
}

#[derive(Debug, Clone)]
pub struct RuleMatch {
    pub(crate) rule_id: String,
    pub(crate) rule_name: String,
    // Only asserted in tests; production consumes the id, name, and reminder.
    #[allow(dead_code)]
    pub(crate) matched_text: String,
    pub(crate) reminder: String,
}

pub(crate) struct StreamRuleMonitor {
    rules: Vec<(StreamRule, regex::Regex)>,
    accumulated_text: String,
    max_window_bytes: usize,
}

impl StreamRuleMonitor {
    pub(crate) fn new(rules: Vec<(StreamRule, regex::Regex)>, config: &AgentConfig) -> Self {
        Self {
            rules,
            accumulated_text: String::new(),
            max_window_bytes: config.stream_rule_max_window_bytes,
        }
    }

    fn clamp_window(&mut self) {
        let max_window = self.max_window_bytes;
        if self.accumulated_text.len() > max_window {
            let mut start = self.accumulated_text.len() - max_window;
            while !self.accumulated_text.is_char_boundary(start) {
                start += 1;
            }
            self.accumulated_text.drain(..start);
        }
    }

    pub(crate) fn push_chunk(&mut self, chunk: &str) -> Option<RuleMatch> {
        self.accumulated_text.push_str(chunk);
        self.clamp_window();
        for (rule, re) in &self.rules {
            if let Some(m) = re.find(&self.accumulated_text) {
                return Some(RuleMatch {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched_text: m.as_str().to_string(),
                    reminder: rule.reminder.clone(),
                });
            }
        }
        None
    }

    pub(crate) fn reset(&mut self) {
        self.accumulated_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_rule_monitor_detects_pattern() {
        let rule = StreamRule {
            id: "box_leak".into(),
            name: "no-box-leak".into(),
            pattern: r"Box::leak\(.*\)".into(),
            reminder: "Do not use Box::leak in production code".into(),
        };
        let re = regex::Regex::new(&rule.pattern).unwrap();

        let config = AgentConfig::default();
        let mut monitor = StreamRuleMonitor::new(vec![(rule, re)], &config);
        assert!(monitor.push_chunk("let x = ").is_none());
        assert!(monitor.push_chunk("Box::leak(").is_none());
        let mat = monitor.push_chunk("ptr);");
        assert!(mat.is_some());
        let m = mat.unwrap();
        assert_eq!(m.rule_id, "box_leak");
        assert_eq!(m.rule_name, "no-box-leak");
        assert_eq!(m.reminder, "Do not use Box::leak in production code");
    }

    #[test]
    fn test_stream_rule_monitor_sliding_window_clamping() {
        let rule = StreamRule {
            id: "secret".into(),
            name: "no-secret".into(),
            pattern: r"PREFIX_SECRET_\d+".into(),
            reminder: "Do not expose secrets".into(),
        };
        let re = regex::Regex::new(&rule.pattern).unwrap();

        let config = AgentConfig::default();
        let mut monitor = StreamRuleMonitor::new(vec![(rule, re)], &config);
        let max_window = config.stream_rule_max_window_bytes;

        // Push partial prefix that does not match by itself
        assert!(monitor.push_chunk("PREFIX_").is_none());

        // Push 5000 bytes of filler
        let filler = "a".repeat(5000);
        assert!(monitor.push_chunk(&filler).is_none());
        assert!(monitor.accumulated_text.len() <= max_window);

        // PREFIX_ was clamped away, so pushing suffix "SECRET_123" will not match
        assert!(monitor.push_chunk("SECRET_123").is_none());

        // Push a complete pattern within the current window range
        let mat = monitor.push_chunk("PREFIX_SECRET_999");
        assert!(mat.is_some());
        assert_eq!(mat.unwrap().matched_text, "PREFIX_SECRET_999");
    }

    #[test]
    fn test_stream_rule_monitor_utf8_boundary_safety() {
        let rule = StreamRule {
            id: "test".into(),
            name: "test-rule".into(),
            pattern: r"TARGET".into(),
            reminder: "Found target".into(),
        };
        let re = regex::Regex::new(&rule.pattern).unwrap();

        let config = AgentConfig::default();
        let mut monitor = StreamRuleMonitor::new(vec![(rule, re)], &config);
        let max_window = config.stream_rule_max_window_bytes;

        // Push multi-byte UTF-8 characters (crab emoji is 4 bytes)
        // 4095 'a' bytes + 1 crab emoji (4 bytes) = 4099 bytes total
        let mut text = "a".repeat(4095);
        text.push('🦀'); // starts at byte 4095, ends at byte 4099
        monitor.push_chunk(&text);

        // Window size before clamp was 4099.
        // target start = 4099 - max_window = 3.
        // Index 3 is char boundary ('a'), so clamped text len = max_window.
        assert!(monitor.accumulated_text.len() <= max_window);

        // Now align so target excess falls INSIDE a multi-byte character
        // 4094 'a's + "🦀" (4 bytes) + "🦀" (4 bytes) = 4102 bytes total
        let mut text2 = "a".repeat(4094);
        text2.push_str("🦀🦀");
        monitor.reset();
        monitor.push_chunk(&text2);

        // 4102 - max_window = 6.
        // Index 6 is inside the first 🦀 (bytes 4094..4098).
        // `start` advances to 4098 (next char boundary).
        // Clamped text len = 4102 - 4098 = 4004 bytes <= max_window.
        assert!(monitor.accumulated_text.len() <= max_window);
        assert!(std::str::from_utf8(monitor.accumulated_text.as_bytes()).is_ok());

        // Match works after clamping
        let mat = monitor.push_chunk("_TARGET_");
        assert!(mat.is_some());
    }

    #[test]
    fn test_classify_tool_replay_safety() {
        assert_eq!(
            classify_tool_replay_safety("view_file"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("list_dir"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("grep_search"),
            ToolReplaySafety::Safe
        );
        assert_eq!(
            classify_tool_replay_safety("replace_file_content"),
            ToolReplaySafety::Never
        );
        assert_eq!(
            classify_tool_replay_safety("run_command"),
            ToolReplaySafety::Never
        );
    }
}
