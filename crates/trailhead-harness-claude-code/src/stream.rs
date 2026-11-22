//! Claude Code's `stream-json` output, read a line at a time into the run's events: steps, tool calls, usage and
//! the subscription's usage windows.

use std::collections::HashSet;

use serde_json::Value;
use trailhead_factory::{ModelId, RunEvent, Usage};

/// Turns the `stream-json` messages into run events and keeps the final result.
pub(crate) struct Stream {
    pub(crate) model: ModelId,
    pub(crate) max_steps: u32,
    pub(crate) steps: u32,
    pub(crate) seen: HashSet<String>,
    pub(crate) result: Option<Value>,
    pub(crate) session_id: Option<String>,
}

impl Stream {
    pub(crate) fn new(model: ModelId, max_steps: u32) -> Stream {
        Stream { model, max_steps, steps: 0, seen: HashSet::new(), result: None, session_id: None }
    }

    pub(crate) fn over_limit(&self) -> bool {
        self.max_steps > 0 && self.steps > self.max_steps
    }

    pub(crate) fn read(&mut self, msg: &Value) -> Vec<RunEvent> {
        let mut out = Vec::new();
        if let Some(id) = msg["session_id"].as_str() {
            self.session_id = Some(id.to_string());
        }
        match msg["type"].as_str() {
            // Claude Code's own messages (errors, notices) are labelled `<synthetic>` and cost nothing.
            Some("assistant") if msg["message"]["model"] == "<synthetic>" => {}
            Some("assistant") => {
                let m = &msg["message"];
                // One model response can arrive as several messages, one per content block, each with its usage.
                let id = m["id"].as_str().unwrap_or_default().to_string();
                if self.seen.insert(id) {
                    self.steps += 1;
                    out.push(RunEvent::StepStarted { step: self.steps });
                    let model = m["model"].as_str().map(|s| ModelId(s.into())).unwrap_or_else(|| self.model.clone());
                    out.push(RunEvent::Usage { model, usage: usage(&m["usage"]) });
                }
                for block in m["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        let name = block["name"].as_str().unwrap_or("unknown").to_string();
                        out.push(RunEvent::ToolCall { name, detail: detail(&block["input"]) });
                    }
                }
            }
            // A tool's result going back to the model: the call is over.
            Some("user") => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        out.push(RunEvent::ToolResult);
                    }
                }
            }
            Some("result") => self.result = Some(msg.clone()),
            Some("rate_limit_event") => out.extend(quota(&msg["rate_limit_info"])),
            _ => {}
        }
        out
    }
}

/// A subscription's usage windows from Claude Code's `rate_limit_event`. Observed, not documented: anything
/// missing or renamed yields no reading, and the factory carries on as if it had none.
pub(crate) fn quota(info: &Value) -> Vec<RunEvent> {
    let Some(windows) = info["unifiedWindows"].as_object() else { return Vec::new() };
    windows
        .iter()
        .filter_map(|(window, w)| {
            let used = w["utilization"].as_f64()?;
            warn_unknown_window(window);
            Some(RunEvent::Quota {
                window: window.clone(),
                used_percent: (used * 100.0).round().clamp(0.0, 1000.0) as u32,
                resets_at: w["resetsAt"].as_i64()?,
            })
        })
        .collect()
}

/// Warns, once per window name per process, that Claude Code reported a usage window `pause_at` can't name: a limit
/// on it would silently never match.
pub(crate) fn warn_unknown_window(window: &str) {
    if first_unknown_window(window) {
        tracing::warn!(
            window,
            known = %trailhead_factory::host::USAGE_WINDOWS.join(", "),
            "claude reported a usage window pause_at can't name; a limit on it would never match"
        );
    }
}

/// True the first time an unrecognized window name is seen.
pub(crate) fn first_unknown_window(window: &str) -> bool {
    static WARNED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);
    if trailhead_factory::host::USAGE_WINDOWS.contains(&window) {
        return false;
    }
    let mut warned = WARNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    warned.get_or_insert_with(HashSet::new).insert(window.to_string())
}

/// What a tool call is about, in one short line: a Bash command, a file path or a search pattern.
pub(crate) fn detail(input: &Value) -> Option<String> {
    let raw =
        ["command", "file_path", "path", "pattern", "url", "description"].iter().find_map(|k| input[*k].as_str())?;
    let line: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if line.chars().count() > 120 { format!("{}…", line.chars().take(120).collect::<String>()) } else { line })
}

pub(crate) fn usage(u: &Value) -> Usage {
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    }
}
