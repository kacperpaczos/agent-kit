//! Codex rollouts: `<sessions>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
//!
//! Each line is `{timestamp, type, payload}` (codex-rs `RolloutLine`). The
//! first is `session_meta` (payload `id`, `cwd`, `timestamp`, `source`,
//! `parent_thread_id`…). Conversation lives in `response_item` lines whose
//! payload is a `ResponseItem` tagged by `type`: `message`,
//! `function_call`, `custom_tool_call`, `local_shell_call` and their
//! `*_output`s. `event_msg` lines repeat the same content for UIs and are
//! skipped.
//!
//! Codex has no skill tool: a tool call that reads `…/<name>/SKILL.md`
//! (see [`crate::infer`]) counts as a [`Certainty::Inferred`] activation.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::infer::skills_read;
use crate::lines::{self, Line};
use crate::time::parse_rfc3339;
use crate::{
    AgentKind, Certainty, EventKind, Filter, Result, Role, SessionInfo, Sink, Warning, mtime,
    summarize,
};

const HEAD_LINES: usize = 8;
const HEAD_LINE_MAX: usize = 4 * 1024 * 1024;
const TAIL_WINDOW: u64 = 64 * 1024;
const MAX_DEPTH: usize = 5;

pub(crate) fn list(
    root: &Path,
    filter: &Filter,
    out: &mut Vec<SessionInfo>,
    warnings: &mut Vec<Warning>,
) {
    let mut files = Vec::new();
    collect(root, 0, &mut files);
    for path in files {
        let modified = mtime(&path);
        if let (Some(since), Some(m)) = (filter.since, modified)
            && m < since
        {
            continue;
        }
        match info(&path, modified) {
            Ok(info) => out.push(info),
            Err(message) => warnings.push(Warning::new(&path, message)),
        }
    }
}

fn collect(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&path, depth + 1, files);
        } else if kind.is_file() && is_rollout(&path) {
            files.push(path);
        }
    }
}

fn is_rollout(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}

fn info(
    path: &Path,
    modified: Option<std::time::SystemTime>,
) -> std::result::Result<SessionInfo, String> {
    let (head, _) = lines::head(path, HEAD_LINES, HEAD_LINE_MAX).map_err(|e| e.to_string())?;
    let Some(meta) = head
        .iter()
        .find(|v| v.get("type").and_then(Value::as_str) == Some("session_meta"))
    else {
        return Err("no session_meta record in the first lines".to_string());
    };
    let payload = meta.get("payload").unwrap_or(&Value::Null);
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
    let id = text("id")
        .or_else(|| text("session_id"))
        .unwrap_or_else(|| id_from_name(path));
    let started = text("timestamp")
        .as_deref()
        .and_then(parse_rfc3339)
        .or_else(|| timestamp(meta));
    let source_subagent = payload
        .get("source")
        .and_then(Value::as_object)
        .is_some_and(|o| o.contains_key("subagent"));
    let parent_id = text("parent_thread_id").or_else(|| {
        payload
            .pointer("/source/subagent/thread_spawn/parent_thread_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let ended = lines::tail(path, TAIL_WINDOW)
        .ok()
        .and_then(|tail| tail.iter().find_map(timestamp))
        .or(modified);
    Ok(SessionInfo {
        agent: AgentKind::Codex,
        id,
        cwd: text("cwd").map(PathBuf::from),
        started,
        ended,
        path: path.to_path_buf(),
        sidechain: source_subagent || parent_id.is_some(),
        parent_id,
        title: None,
    })
}

/// The uuid at the end of `rollout-<ts>-<uuid>.jsonl`.
fn id_from_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A uuid is 36 chars (8-4-4-4-12).
    if stem.len() > 36 && stem.is_char_boundary(stem.len() - 36) {
        stem[stem.len() - 36..].to_string()
    } else {
        stem
    }
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
            Ok(record) => {
                if record.get("type").and_then(Value::as_str) == Some("response_item") {
                    let payload = record.get("payload").unwrap_or(&Value::Null);
                    item_events(payload, timestamp(&record), sink);
                }
            }
            Err(e) => sink.warn(format!("line {line_no}: broken JSON ({e})")),
        }
    }
    Ok(())
}

fn item_events(item: &Value, ts: Option<std::time::SystemTime>, sink: &mut Sink<'_>) {
    let str_of = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
    match str_of("type") {
        "message" => {
            let role = match str_of("role") {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => Role::System,
            };
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                return;
            };
            for part in content {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    let text = sink.text(text);
                    sink.push(ts, role, EventKind::Text(text));
                }
            }
        }
        "function_call" => {
            let args = str_of("arguments");
            tool_call(sink, ts, str_of("name"), args, sink.json_or_text(args));
        }
        "custom_tool_call" => {
            let input = str_of("input");
            tool_call(sink, ts, str_of("name"), input, sink.text(input));
        }
        "local_shell_call" => {
            let command = item.pointer("/action/command").unwrap_or(&Value::Null);
            let shown = match command {
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                other => summarize(other),
            };
            let shown = sink.text(&shown);
            tool_call(sink, ts, "local_shell", &command.to_string(), shown);
        }
        "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
            let raw = output_text(item.get("output"));
            let is_error = output_failed(&raw);
            let summary = sink.text(&raw);
            sink.push(ts, Role::Tool, EventKind::ToolResult { is_error, summary });
        }
        // reasoning, web_search_call, compaction…
        _ => {}
    }
}

/// A tool use, plus an inferred activation for each skill whose
/// `SKILL.md` the call reads (see [`crate::infer`]).
fn tool_call(
    sink: &mut Sink<'_>,
    ts: Option<std::time::SystemTime>,
    name: &str,
    raw_input: &str,
    input_summary: String,
) {
    let kind = EventKind::ToolUse {
        name: sink.text(name),
        input_summary,
    };
    sink.push(ts, Role::Assistant, kind);
    for skill in skills_read(name, raw_input) {
        let name = sink.text(&skill);
        sink.push(
            ts,
            Role::Assistant,
            EventKind::SkillActivation {
                name,
                certainty: Certainty::Inferred,
            },
        );
    }
}

/// A `FunctionCallOutputPayload`: a string or an array of content items.
fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        // Older rollouts: `{content, success}`.
        Some(Value::Object(o)) => o
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Value::Object(o.clone()).to_string()),
        Some(other) => summarize(other),
        None => String::new(),
    }
}

/// Whether a shell output reports a non-zero exit code, either as JSON
/// `{"metadata":{"exit_code":N}}` or as an `Exit code: N` /
/// `Process exited with code N` line.
fn output_failed(raw: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && let Some(code) = v.pointer("/metadata/exit_code").and_then(Value::as_i64)
    {
        return code != 0;
    }
    raw.lines().take(8).any(|line| {
        let line = line.trim();
        ["Exit code:", "Process exited with code"]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .and_then(|rest| rest.trim().parse::<i64>().ok())
            .is_some_and(|code| code != 0)
    })
}

fn timestamp(v: &Value) -> Option<std::time::SystemTime> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)
}
