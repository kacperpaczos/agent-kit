//! Parser for the Codex `exec --json` stream (spec §6.2, shapes from the
//! OpenDesign recording `codex/turn-exec-json.jsonl`).

use std::path::PathBuf;

use crate::event::AgentEvent;
use crate::transport::LineParser;

/// A line-by-line parser for `codex exec --json` output.
pub struct CodexParser {
    cwd: PathBuf,
}

impl CodexParser {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    /// `path` as a target string, relative to the working copy when possible.
    fn target(&self, path: &str) -> String {
        let p = PathBuf::from(path);
        match p.strip_prefix(&self.cwd) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => path.to_string(),
        }
    }
}

/// An error message mentioning account/login problems is a login problem.
fn is_auth_message(message: &str) -> bool {
    crate::event::is_login_message(message)
}

fn error_event(message: String) -> AgentEvent {
    if is_auth_message(&message) {
        AgentEvent::NeedsLogin { message }
    } else {
        AgentEvent::Error { message }
    }
}
fn failure_message(value: &serde_json::Value) -> String {
    let err = value.get("error").or_else(|| value.get("message"));
    match err {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| other.to_string()),
        None => "turn.failed".to_string(),
    }
}

impl LineParser for CodexParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        let Some(kind) = value.get("type").and_then(|t| t.as_str()) else {
            return Vec::new();
        };
        match kind {
            "thread.started" => match value.get("thread_id").and_then(|t| t.as_str()) {
                Some(id) => vec![AgentEvent::SessionStarted { id: id.to_string() }],
                None => Vec::new(),
            },
            "turn.completed" => vec![AgentEvent::TurnEnd { ok: true }],
            "turn.failed" => vec![
                error_event(failure_message(&value)),
                AgentEvent::TurnEnd { ok: false },
            ],
            "item.completed" => {
                let Some(item) = value.get("item") else {
                    return Vec::new();
                };
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("agent_message") => match item.get("text").and_then(|t| t.as_str()) {
                        Some(text) => vec![AgentEvent::Text {
                            text: text.to_string(),
                        }],
                        None => Vec::new(),
                    },
                    Some("command_execution") => vec![AgentEvent::ToolCall {
                        name: "command_execution".to_string(),
                        target: None,
                    }],
                    Some("file_change") => {
                        let target = item
                            .get("changes")
                            .and_then(|c| c.as_array())
                            .and_then(|cs| cs.first())
                            .and_then(|c| c.get("path"))
                            .and_then(|p| p.as_str())
                            .map(|p| self.target(p));
                        vec![AgentEvent::ToolCall {
                            name: "file_change".to_string(),
                            target,
                        }]
                    }
                    Some("error") => {
                        match item.get("message").and_then(|m| m.as_str()) {
                            // Advisory warnings (shortened skill descriptions)
                            // are not failures.
                            Some(m) if m.contains("Skill descriptions were shortened") => {
                                Vec::new()
                            }
                            Some(m) => vec![error_event(m.to_string())],
                            None => Vec::new(),
                        }
                    }
                    // `reasoning` and anything unknown: dropped.
                    _ => Vec::new(),
                }
            }
            // Top-level error frame: a real error, unless it is reconnect
            // progress chatter.
            "error" => {
                let message = value.get("message").and_then(|m| m.as_str()).unwrap_or("");
                if message.is_empty() || message.starts_with("Reconnecting") {
                    Vec::new()
                } else {
                    vec![error_event(message.to_string())]
                }
            }
            _ => Vec::new(),
        }
    }
}
