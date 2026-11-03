//! What one Claude Code run reported, read from its raw
//! `--output-format stream-json` lines: tool calls from `assistant`
//! messages, and the closing `result` event (usage with the cache split,
//! `total_cost_usd`, `num_turns`, `duration_ms`, the answer). The app's
//! parser (`theodore_agents::claude`) normalizes these away, so the eval
//! reads the lines itself.

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamStats {
    pub tool_calls: BTreeMap<String, u32>,
    /// The `result` event came.
    pub finished: bool,
    pub is_error: bool,
    /// `result`: the answer, or the error.
    pub final_text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<u32>,
    pub duration_ms: Option<u64>,
    /// Tokens summed over `assistant` messages: what a run that never
    /// reached `result` (killed) still spent.
    pub partial_tokens: u64,
}

impl StreamStats {
    /// Takes one line; anything that isn't a known event is skipped.
    pub fn on_line(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.on_assistant(&v),
            Some("result") => self.on_result(&v),
            _ => {}
        }
    }

    fn on_assistant(&mut self, v: &Value) {
        if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
            for b in blocks {
                if b.get("type").and_then(Value::as_str) == Some("tool_use")
                    && let Some(name) = b.get("name").and_then(Value::as_str)
                {
                    *self.tool_calls.entry(name.to_string()).or_default() += 1;
                }
            }
        }
        if let Some(u) = v.pointer("/message/usage") {
            let f = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.partial_tokens += f("input_tokens")
                + f("output_tokens")
                + f("cache_creation_input_tokens")
                + f("cache_read_input_tokens");
        }
    }

    fn on_result(&mut self, v: &Value) {
        self.finished = true;
        self.is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        self.final_text = v
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                v.get("errors").and_then(Value::as_array).map(|e| {
                    e.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            })
            .unwrap_or_default();
        if let Some(u) = v.get("usage") {
            let f = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.input_tokens = f("input_tokens");
            self.output_tokens = f("output_tokens");
            self.cache_creation_tokens = f("cache_creation_input_tokens");
            self.cache_read_tokens = f("cache_read_input_tokens");
        }
        self.cost_usd = v.get("total_cost_usd").and_then(Value::as_f64);
        self.num_turns = v
            .get("num_turns")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        self.duration_ms = v.get("duration_ms").and_then(Value::as_u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tools_and_reads_the_result() {
        let mut s = StreamStats::default();
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["Read"]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"looking"},{"type":"tool_use","id":"1","name":"Grep","input":{}}],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"1","content":"x"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"2","name":"mcp__code-tools__find_symbol","input":{"name":"save"}},{"type":"tool_use","id":"3","name":"Grep","input":{}}]}}"#,
            "not json",
            r#"{"type":"result","subtype":"success","is_error":false,"result":"It is in core.rs","num_turns":3,"duration_ms":4200,"total_cost_usd":0.0123,"usage":{"input_tokens":12,"output_tokens":80,"cache_creation_input_tokens":900,"cache_read_input_tokens":3000}}"#,
        ] {
            s.on_line(line);
        }
        assert_eq!(s.tool_calls["Grep"], 2);
        assert_eq!(s.tool_calls["mcp__code-tools__find_symbol"], 1);
        assert!(s.finished && !s.is_error);
        assert_eq!(s.final_text, "It is in core.rs");
        assert_eq!(
            (
                s.input_tokens,
                s.output_tokens,
                s.cache_creation_tokens,
                s.cache_read_tokens
            ),
            (12, 80, 900, 3000)
        );
        assert_eq!(s.cost_usd, Some(0.0123));
        assert_eq!(s.num_turns, Some(3));
        assert_eq!(s.duration_ms, Some(4200));
        assert_eq!(s.partial_tokens, 115);
    }

    #[test]
    fn an_error_result_keeps_its_reason() {
        let mut s = StreamStats::default();
        s.on_line(
            r#"{"type":"result","is_error":true,"num_turns":0,"total_cost_usd":0,"errors":["boom"]}"#,
        );
        assert!(s.finished && s.is_error);
        assert_eq!(s.final_text, "boom");
        assert_eq!(s.cost_usd, Some(0.0));
    }
}
