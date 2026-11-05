//! Claude Code transcripts: `<projects>/<cwd-slug>/<uuid>.jsonl`, with
//! subagent transcripts in `<uuid>/subagents/*.jsonl`.
//!
//! Each line is one record with `type`, `sessionId`, `cwd`, `timestamp`,
//! `isSidechain`, `message`… Only `user` and `assistant` records carry
//! conversation; the rest (`attachment`, `api-request*`,
//! `file-history-snapshot`, `summary`, `mode`…) are skipped.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::lines::{self, Line};
use crate::time::parse_rfc3339;
use crate::{
    AgentKind, Certainty, EventKind, Filter, Result, Role, SessionInfo, Sink, Warning, mtime,
    summarize,
};

const HEAD_LINES: usize = 50;
const HEAD_LINE_MAX: usize = 1024 * 1024;
const TAIL_WINDOW: u64 = 64 * 1024;

pub(crate) fn list(
    root: &Path,
    filter: &Filter,
    out: &mut Vec<SessionInfo>,
    warnings: &mut Vec<Warning>,
) {
    let Ok(projects) = fs::read_dir(root) else {
        return;
    };
    for project in projects.flatten() {
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_jsonl(&path) {
                consider(&path, None, filter, out, warnings);
            } else if path.is_dir() {
                let parent = file_stem(&path);
                let Ok(subs) = fs::read_dir(path.join("subagents")) else {
                    continue;
                };
                for sub in subs.flatten() {
                    let sub = sub.path();
                    if is_jsonl(&sub) {
                        consider(&sub, Some(parent.clone()), filter, out, warnings);
                    }
                }
            }
        }
    }
}

fn consider(
    path: &Path,
    parent: Option<String>,
    filter: &Filter,
    out: &mut Vec<SessionInfo>,
    warnings: &mut Vec<Warning>,
) {
    if parent.is_some() && !filter.include_sidechains {
        return;
    }
    let modified = mtime(path);
    // A file last written before `since` cannot hold anything newer.
    if let (Some(since), Some(m)) = (filter.since, modified)
        && m < since
    {
        return;
    }
    match info(path, parent, modified) {
        Ok(Some(info)) => out.push(info),
        Ok(None) => {}
        Err(message) => warnings.push(Warning::new(path, message)),
    }
}

fn info(
    path: &Path,
    parent: Option<String>,
    modified: Option<std::time::SystemTime>,
) -> std::result::Result<Option<SessionInfo>, String> {
    let (head, broken) = lines::head(path, HEAD_LINES, HEAD_LINE_MAX).map_err(|e| e.to_string())?;
    if head.is_empty() {
        return if broken > 0 {
            Err("no readable JSON in the first lines".to_string())
        } else {
            Ok(None)
        };
    }
    let started = head.iter().find_map(timestamp);
    let cwd = head
        .iter()
        .find_map(|v| v.get("cwd").and_then(Value::as_str))
        .map(PathBuf::from);
    let marked_sidechain = head
        .iter()
        .find_map(|v| v.get("isSidechain").and_then(Value::as_bool))
        .unwrap_or(false);
    let ended = lines::tail(path, TAIL_WINDOW)
        .ok()
        .and_then(|tail| tail.iter().find_map(timestamp))
        .or(modified);
    Ok(Some(SessionInfo {
        agent: AgentKind::Claude,
        id: file_stem(path),
        cwd,
        started,
        ended,
        path: path.to_path_buf(),
        sidechain: parent.is_some() || marked_sidechain,
        parent_id: parent,
        title: None,
    }))
}

pub(crate) fn read(path: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let io_err = |source| crate::Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut reader = lines::open(path, sink.max_line()).map_err(io_err)?;
    let mut line_no = 0usize;
    while !sink.full() {
        let Some(line) = reader.next_line().map_err(io_err)? else {
            break;
        };
        line_no += 1;
        let bytes = match line {
            Line::Text(b) if b.iter().all(u8::is_ascii_whitespace) => continue,
            Line::Text(b) => b,
            Line::TooLong => {
                sink.warn(format!(
                    "line {line_no}: longer than the line limit, skipped"
                ));
                continue;
            }
        };
        match serde_json::from_slice::<Value>(bytes) {
            Ok(record) => record_events(&record, sink),
            Err(e) => sink.warn(format!("line {line_no}: broken JSON ({e})")),
        }
    }
    Ok(())
}

fn record_events(record: &Value, sink: &mut Sink<'_>) {
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    let role = match kind {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return,
    };
    let ts = timestamp(record);
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    match content {
        Value::String(text) => push_text(sink, ts, role, text),
        Value::Array(items) => {
            for item in items {
                item_events(item, ts, role, sink);
            }
        }
        _ => {}
    }
}

fn item_events(item: &Value, ts: Option<std::time::SystemTime>, role: Role, sink: &mut Sink<'_>) {
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "text" => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                push_text(sink, ts, role, text);
            }
        }
        "tool_use" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            let input = item.get("input").unwrap_or(&Value::Null);
            if name == "Skill"
                && let Some(skill) = skill_name(input)
            {
                let name = sink.text(&skill);
                sink.push(
                    ts,
                    Role::Assistant,
                    EventKind::SkillActivation {
                        name,
                        certainty: Certainty::Certain,
                    },
                );
                return;
            }
            let kind = EventKind::ToolUse {
                name: sink.text(name),
                input_summary: sink.text(&summarize(input)),
            };
            sink.push(ts, Role::Assistant, kind);
        }
        "tool_result" => {
            let is_error = item
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let summary = sink.text(&result_text(item.get("content")));
            sink.push(ts, Role::Tool, EventKind::ToolResult { is_error, summary });
        }
        // thinking, redacted_thinking, image, document…
        _ => {}
    }
}

/// The skill named by a `Skill` tool input: `skill` (current) or
/// `command` (older releases); a leading `/` is dropped.
fn skill_name(input: &Value) -> Option<String> {
    ["skill", "command", "name"]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(|s| s.trim().trim_start_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

/// Text of a `tool_result` content: a string or an array of blocks.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => summarize(other),
        None => String::new(),
    }
}

fn push_text(sink: &mut Sink<'_>, ts: Option<std::time::SystemTime>, role: Role, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let text = sink.text(text);
    sink.push(ts, role, EventKind::Text(text));
}

fn timestamp(v: &Value) -> Option<std::time::SystemTime> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jsonl") && path.is_file()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
