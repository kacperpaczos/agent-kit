//! Line parser for Claude Code's `--output-format stream-json --verbose`
//! JSONL stream, modeled on OpenDesign's `claude-stream.ts` but reduced to
//! SkillCraft's single event vocabulary. Every line maps to zero or more
//! `AgentEvent`s; lines that carry nothing the UI needs (hook chatter,
//! thinking blocks, tool results, rate-limit events) map to nothing.

use std::path::{Path, PathBuf};

use crate::event::AgentEvent;
use crate::transport::LineParser;

/// Parses one Claude stream-json stdout line.
pub struct ClaudeParser {
    cwd: PathBuf,
}

impl ClaudeParser {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

/// A `result` frame whose text means "the agent is not logged in", which the
/// UI must present as `NeedsLogin`, not a generic error.
fn is_login_error(text: &str) -> bool {
    crate::event::is_login_message(text)
}

/// Read the write target out of a `tool_use` input object: `file_path`
/// resolved against the parser's cwd, else `path`/`pattern` verbatim.
fn tool_target(input: &serde_json::Value, cwd: &Path) -> Option<String> {
    if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str()) {
        let path = Path::new(file_path);
        if let Ok(rel) = path.strip_prefix(cwd) {
            return Some(rel.to_string_lossy().into_owned());
        }
        return Some(file_path.to_string());
    }
    for key in ["path", "pattern"] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

impl LineParser for ClaudeParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("system") => {
                if obj.get("subtype").and_then(|s| s.as_str()) == Some("init")
                    && let Some(id) = obj.get("session_id").and_then(|v| v.as_str())
                {
                    events.push(AgentEvent::SessionStarted { id: id.to_string() });
                }
            }
            Some("assistant") => {
                if let Some(content) = obj
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                    events.push(AgentEvent::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            Some("tool_use") => {
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let target = tool_target(
                                    block.get("input").unwrap_or(&serde_json::Value::Null),
                                    &self.cwd,
                                );
                                events.push(AgentEvent::ToolCall {
                                    name: name.to_string(),
                                    target,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("result") => {
                let is_error = obj
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_error {
                    // Message precedence (OpenDesign `errorResultMessage`):
                    // non-empty `errors[]`, then `result`, then the subtype —
                    // never an empty message.
                    let errors: Vec<&str> = obj
                        .get("errors")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    let result = obj
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let subtype = obj
                        .get("subtype")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let message = if !errors.is_empty() {
                        errors.join("; ")
                    } else if !result.is_empty() {
                        result.to_string()
                    } else if !subtype.is_empty() {
                        format!("Claude run failed: {subtype}")
                    } else {
                        "Claude run failed".to_string()
                    };
                    if is_login_error(&message) {
                        events.push(AgentEvent::NeedsLogin { message });
                    } else {
                        events.push(AgentEvent::Error { message });
                    }
                }
                events.push(AgentEvent::TurnEnd { ok: !is_error });
            }
            _ => {}
        }
        events
    }
}
