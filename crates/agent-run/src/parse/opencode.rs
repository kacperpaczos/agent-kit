//! OpenCode `run --format json` line parser (plan Task 5).
//!
//! Event shapes follow OpenDesign's `json-event-stream.ts`
//! (`handleOpenCodeEvent`): OpenCode stamps `sessionID` on every stream
//! event, so the parser reports it once, on first sight.

use std::path::{Path, PathBuf};

use crate::event::AgentEvent;
use crate::transport::LineParser;

/// Parses one stdout line of `opencode run --format json` into events.
pub struct OpencodeParser {
    cwd: PathBuf,
    session_reported: bool,
    seen_calls: std::collections::HashSet<String>,
}

impl OpencodeParser {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            session_reported: false,
            seen_calls: Default::default(),
        }
    }
}

/// Tool target: `filePath`/`file_path`/`path` from the tool input, made
/// relative to the working copy when it points inside it.
fn tool_target(input: &serde_json::Value, cwd: &Path) -> Option<String> {
    let raw = input
        .get("filePath")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))?
        .as_str()?;
    let p = Path::new(raw);
    if p.is_absolute() {
        Some(
            p.strip_prefix(cwd)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        Some(raw.to_string())
    }
}

fn is_login_message(message: &str) -> bool {
    crate::event::is_login_message(message)
}

/// Error text from a string or a structured error frame
/// (`{"name":…,"data":{"message":…}}` or `{"message":…}`).
fn extract_message(value: &serde_json::Value) -> Option<String> {
    let s = match value {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Object(map) => map
            .get("data")
            .and_then(|d| d.get("message"))
            .or_else(|| map.get("message"))
            .or_else(|| map.get("name"))
            .and_then(|m| m.as_str())?,
        _ => return None,
    };
    (!s.is_empty()).then(|| s.to_string())
}

impl LineParser for OpencodeParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        let mut events = Vec::new();

        // OpenCode stamps `sessionID` on every event; report it once, from
        // whichever event carries it first (OpenDesign json-event-stream.ts).
        if !self.session_reported
            && obj
                .get("sessionID")
                .and_then(|v| v.as_str())
                .is_some_and(|id| !id.is_empty())
        {
            let id = obj.get("sessionID").and_then(|v| v.as_str()).unwrap();
            self.session_reported = true;
            events.push(AgentEvent::SessionStarted { id: id.to_string() });
        }

        let part = obj.get("part").cloned().unwrap_or(serde_json::Value::Null);
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = part
                    .get("text")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.is_empty())
                {
                    events.push(AgentEvent::Text {
                        text: text.to_string(),
                    });
                }
            }
            Some("tool_use") => {
                if let Some(tool) = part.get("tool").and_then(|t| t.as_str()) {
                    // OpenCode re-emits tool parts on state updates; one
                    // `ToolCall` per `callID`.
                    if let Some(call) = part.get("callID").and_then(|c| c.as_str())
                        && !self.seen_calls.insert(call.to_string())
                    {
                        return events;
                    }
                    let raw = part
                        .get("state")
                        .and_then(|s| s.get("input"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    // Some tools report `state.input` as a JSON string.
                    let input = match &raw {
                        serde_json::Value::String(s) => {
                            serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
                        }
                        other => other.clone(),
                    };
                    events.push(AgentEvent::ToolCall {
                        name: tool.to_string(),
                        target: tool_target(&input, &self.cwd),
                    });
                }
            }
            Some("error") => {
                let message = obj
                    .get("error")
                    .or_else(|| obj.get("message"))
                    .and_then(extract_message)
                    .unwrap_or_else(|| "OpenCode error".to_string());
                if is_login_message(&message) {
                    events.push(AgentEvent::NeedsLogin { message });
                } else {
                    events.push(AgentEvent::Error { message });
                }
            }
            // OpenCode finishes one step per model call (tool calls make several
            // per turn) and ends the turn by exiting, like OpenDesign assumes:
            // `run_turn` emits `TurnEnd` when the process ends cleanly.
            Some("step_finish") => {}
            _ => {}
        }
        events
    }
}
