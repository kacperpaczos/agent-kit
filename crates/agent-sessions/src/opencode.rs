//! OpenCode sessions in `opencode.db` (SQLite, Drizzle schema of
//! `packages/core/src/session/sql.ts`):
//!
//! - `session_v2(id, directory, title, parent_id, time_created, time_updated, …)`;
//! - `session_message(id, session_id, type, seq, time_created, data)` where
//!   `data` is the JSON message without `type`/`id`; `type` is one of
//!   `user`, `assistant`, `synthetic`, `system`, `skill`, `shell`, …;
//! - optionally `session_part(message_id, ordinal, type, data)` holding an
//!   assistant message's content (`text` / `reasoning` / `tool`) in newer
//!   releases; older ones keep it in the message's `data.content`.
//!
//! The database is opened read-only (`file:…?mode=ro`, busy timeout, never
//! `immutable`). Oversized rows are skipped by their stored length before
//! loading. An unknown layout or a locked database is reported, never a
//! panic.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use crate::time::from_millis;
use crate::{
    AgentKind, Certainty, Error, EventKind, Result, Role, SessionInfo, Sink, Warning, summarize,
};

struct Schema {
    has_title: bool,
    has_parent: bool,
    has_parts: bool,
}

pub(crate) fn list(db: &Path, out: &mut Vec<SessionInfo>, warnings: &mut Vec<Warning>) {
    if !db.is_file() {
        return;
    }
    if let Err(err) = list_inner(db, out) {
        warnings.push(Warning::new(db, err.to_string()));
    }
}

fn list_inner(db: &Path, out: &mut Vec<SessionInfo>) -> Result<()> {
    let conn = connect(db)?;
    let schema = schema(&conn, db)?;
    let sql = format!(
        "SELECT id, directory, time_created, time_updated, {}, {} FROM session_v2",
        if schema.has_title { "title" } else { "NULL" },
        if schema.has_parent {
            "parent_id"
        } else {
            "NULL"
        },
    );
    let db_err = |e: rusqlite::Error| database(db, e);
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(db_err)?;
    for row in rows {
        let (id, directory, created, updated, title, parent) = row.map_err(db_err)?;
        out.push(SessionInfo {
            agent: AgentKind::OpenCode,
            id,
            cwd: directory.filter(|d| !d.is_empty()).map(PathBuf::from),
            started: created.and_then(from_millis),
            ended: updated.and_then(from_millis),
            path: db.to_path_buf(),
            sidechain: parent.is_some(),
            parent_id: parent,
            title: title.filter(|t| !t.is_empty()),
        });
    }
    Ok(())
}

pub(crate) fn read(db: &Path, session: &str, sink: &mut Sink<'_>) -> Result<()> {
    let conn = connect(db)?;
    let schema = schema(&conn, db)?;
    let db_err = |e: rusqlite::Error| database(db, e);
    // `octet_length` comes from the record header, so an oversized value is
    // never loaded: the CASE yields NULL for it instead.
    let mut stmt = conn
        .prepare(
            "SELECT id, type, time_created, octet_length(data), \
                    CASE WHEN octet_length(data) <= ?2 THEN data END \
             FROM session_message WHERE session_id = ?1 ORDER BY seq",
        )
        .map_err(db_err)?;
    let mut parts = if schema.has_parts {
        Some(
            conn.prepare(
                "SELECT ordinal, octet_length(data), \
                        CASE WHEN octet_length(data) <= ?2 THEN data END \
                 FROM session_part WHERE message_id = ?1 ORDER BY ordinal",
            )
            .map_err(db_err)?,
        )
    } else {
        None
    };
    let max = i64::try_from(sink.max_line()).unwrap_or(i64::MAX);
    let mut rows = stmt.query(params![session, max]).map_err(db_err)?;
    while !sink.full() {
        let Some(row) = rows.next().map_err(db_err)? else {
            break;
        };
        // Ids are used as-is (part lookup), so an id that is too long or
        // not UTF-8 rejects the row instead of being shortened.
        let seq = sink_row_label(row);
        let id = match exact_text(row, 0).map_err(db_err)? {
            Ok(id) => id,
            Err(why) => {
                sink.warn(format!("message {seq}: id {why}, skipped"));
                continue;
            }
        };
        let kind = match exact_text(row, 1).map_err(db_err)? {
            Ok(kind) => kind,
            Err(why) => {
                sink.warn(format!("message {id}: type {why}, skipped"));
                continue;
            }
        };
        let ts = row
            .get::<_, Option<i64>>(2)
            .map_err(db_err)?
            .and_then(from_millis);
        let data = match json_cell(row, 3, 4, sink.max_line()).map_err(db_err)? {
            Cell::Json(v) => v,
            Cell::TooLarge(len) => {
                sink.warn(format!(
                    "message {id}: {len} bytes, over the line limit, skipped"
                ));
                continue;
            }
            Cell::Broken(e) => {
                sink.warn(format!("message {id}: broken JSON ({e})"));
                continue;
            }
        };
        match kind.as_str() {
            "user" => text_event(sink, ts, Role::User, &data),
            "synthetic" | "system" => text_event(sink, ts, Role::System, &data),
            "skill" => {
                if let Some(name) = data
                    .get("name")
                    .or_else(|| data.get("skill"))
                    .and_then(Value::as_str)
                {
                    skill_event(sink, ts, name);
                }
            }
            "shell" => {
                let command = data.get("command").and_then(Value::as_str).unwrap_or("");
                let kind = EventKind::ToolUse {
                    name: "shell".to_string(),
                    input_summary: sink.text(command),
                };
                sink.push(ts, Role::User, kind);
                if let Some(exit) = data.get("exit").and_then(Value::as_i64) {
                    let summary = sink.json(data.get("output").unwrap_or(&Value::Null));
                    sink.push(
                        ts,
                        Role::Tool,
                        EventKind::ToolResult {
                            is_error: exit != 0,
                            summary,
                        },
                    );
                }
            }
            "assistant" => {
                let inline = data
                    .get("content")
                    .and_then(Value::as_array)
                    .filter(|c| !c.is_empty());
                match (inline, parts.as_mut()) {
                    (Some(content), _) => {
                        for item in content {
                            content_events(sink, ts, item);
                        }
                    }
                    (None, Some(parts)) => {
                        let mut part_rows = parts.query(params![id, max]).map_err(db_err)?;
                        while !sink.full() {
                            let Some(part) = part_rows.next().map_err(db_err)? else {
                                break;
                            };
                            let ordinal = part.get::<_, Option<i64>>(0).map_err(db_err)?;
                            let ordinal = ordinal.unwrap_or(-1);
                            match json_cell(part, 1, 2, sink.max_line()).map_err(db_err)? {
                                Cell::Json(item) => content_events(sink, ts, &item),
                                Cell::TooLarge(len) => sink.warn(format!(
                                    "part {ordinal} of {id}: {len} bytes, over the line limit, skipped"
                                )),
                                Cell::Broken(e) => sink.warn(format!(
                                    "part {ordinal} of {id}: broken JSON ({e})"
                                )),
                            }
                        }
                    }
                    (None, None) => {}
                }
            }
            // agent-switched, model-switched, compaction, idle…
            _ => {}
        }
    }
    Ok(())
}

fn content_events(sink: &mut Sink<'_>, ts: Option<std::time::SystemTime>, item: &Value) {
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "text" => text_event(sink, ts, Role::Assistant, item),
        "tool" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            let state = item.get("state").unwrap_or(&Value::Null);
            let input = state.get("input").unwrap_or(&Value::Null);
            if name == "skill"
                && let Some(skill) = input
                    .get("name")
                    .or_else(|| input.get("skill"))
                    .and_then(Value::as_str)
            {
                skill_event(sink, ts, skill);
            } else {
                let kind = EventKind::ToolUse {
                    name: sink.text(name),
                    input_summary: sink.json(input),
                };
                sink.push(ts, Role::Assistant, kind);
            }
            let status = state.get("status").and_then(Value::as_str).unwrap_or("");
            if matches!(status, "completed" | "error") {
                let mut text = content_text(state.get("content"));
                if status == "error" && text.is_empty() {
                    text = state
                        .get("error")
                        .map(|e| {
                            e.get("message")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| summarize(e))
                        })
                        .unwrap_or_default();
                }
                let summary = sink.text(&text);
                sink.push(
                    ts,
                    Role::Tool,
                    EventKind::ToolResult {
                        is_error: status == "error",
                        summary,
                    },
                );
            }
        }
        // reasoning
        _ => {}
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn text_event(sink: &mut Sink<'_>, ts: Option<std::time::SystemTime>, role: Role, data: &Value) {
    if let Some(text) = data.get("text").and_then(Value::as_str)
        && !text.trim().is_empty()
    {
        let text = sink.text(text);
        sink.push(ts, role, EventKind::Text(text));
    }
}

fn skill_event(sink: &mut Sink<'_>, ts: Option<std::time::SystemTime>, name: &str) {
    let name = sink.text(name);
    sink.push(
        ts,
        Role::Assistant,
        EventKind::SkillActivation {
            name,
            certainty: Certainty::Certain,
        },
    );
}

/// How long a reader waits for a writer's lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Open with `mode=ro` and a busy timeout. A database that is locked,
/// unreadable or not SQLite is an error (reported as a warning by
/// listing); the reader never claims `immutable`, which would skip
/// SQLite's locking on a database the agent may be writing.
fn connect(db: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let uri = format!("file:{}?mode=ro", uri_path(db));
    Connection::open_with_flags(&uri, flags)
        .and_then(|conn| {
            conn.busy_timeout(BUSY_TIMEOUT)?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
            Ok(conn)
        })
        .map_err(|e| database(db, e))
}

enum Cell {
    Json(Value),
    TooLarge(i64),
    Broken(String),
}

/// A JSON column fetched as `octet_length(col)` at `len_idx` and
/// `CASE WHEN octet_length(col) <= max THEN col END` at `data_idx`; the
/// borrowed value is parsed in place, never copied whole.
fn json_cell(
    row: &rusqlite::Row<'_>,
    len_idx: usize,
    data_idx: usize,
    max: usize,
) -> rusqlite::Result<Cell> {
    let len = row.get::<_, Option<i64>>(len_idx)?.unwrap_or(0);
    let bytes = match row.get_ref(data_idx)? {
        ValueRef::Text(b) | ValueRef::Blob(b) => b,
        ValueRef::Null if len > 0 => return Ok(Cell::TooLarge(len)),
        _ => return Ok(Cell::Broken("not text".to_string())),
    };
    if bytes.len() > max {
        return Ok(Cell::TooLarge(len));
    }
    Ok(match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => Cell::Json(v),
        Err(e) => Cell::Broken(e.to_string()),
    })
}

/// Longest id / type accepted from the database.
const MAX_ID: usize = 1024;

/// An id or type column, exactly as stored; `Err(reason)` when it is too
/// long, not UTF-8 or missing. Never shortened.
fn exact_text(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<std::result::Result<String, String>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Text(b) | ValueRef::Blob(b) if b.len() > MAX_ID => {
            Err(format!("longer than {MAX_ID} bytes ({} bytes)", b.len()))
        }
        ValueRef::Text(b) | ValueRef::Blob(b) => std::str::from_utf8(b)
            .map(str::to_string)
            .map_err(|_| "not UTF-8".to_string()),
        ValueRef::Integer(i) => Ok(i.to_string()),
        _ => Err("missing".to_string()),
    })
}

/// A label for a row whose id cannot be used: its position in the result.
fn sink_row_label(row: &rusqlite::Row<'_>) -> String {
    // `rowid` is not selected; the time column is a stable enough hint.
    match row.get_ref(2) {
        Ok(ValueRef::Integer(t)) => format!("at {t}"),
        _ => "(unknown)".to_string(),
    }
}

/// Percent-encode the characters that end or escape a URI path.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            c => out.push(c),
        }
    }
    out
}

fn schema(conn: &Connection, db: &Path) -> Result<Schema> {
    let columns = |table: &str| -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(|e| database(db, e))?;
        let names = stmt
            .query_map(params![table], |row| row.get::<_, String>(0))
            .map_err(|e| database(db, e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| database(db, e))?;
        Ok(names)
    };
    let unsupported = |message: String| Error::UnsupportedSchema {
        path: db.to_path_buf(),
        message,
    };
    let sessions = columns("session_v2")?;
    if sessions.is_empty() {
        let legacy = !columns("session")?.is_empty();
        return Err(unsupported(if legacy {
            "legacy layout (table `session`, no `session_v2`)".to_string()
        } else {
            "no `session_v2` table".to_string()
        }));
    }
    let missing = |have: &[String], need: &[&str]| -> Vec<String> {
        need.iter()
            .filter(|n| !have.iter().any(|h| h == *n))
            .map(|n| n.to_string())
            .collect()
    };
    let gone = missing(
        &sessions,
        &["id", "directory", "time_created", "time_updated"],
    );
    if !gone.is_empty() {
        return Err(unsupported(format!(
            "`session_v2` lacks {}",
            gone.join(", ")
        )));
    }
    let messages = columns("session_message")?;
    let gone = missing(
        &messages,
        &["id", "session_id", "type", "seq", "time_created", "data"],
    );
    if !gone.is_empty() {
        return Err(unsupported(format!(
            "`session_message` lacks {}",
            gone.join(", ")
        )));
    }
    let parts = columns("session_part")?;
    Ok(Schema {
        has_title: sessions.iter().any(|c| c == "title"),
        has_parent: sessions.iter().any(|c| c == "parent_id"),
        has_parts: missing(&parts, &["message_id", "ordinal", "data"]).is_empty(),
    })
}

fn database(db: &Path, e: rusqlite::Error) -> Error {
    let busy = matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    );
    Error::Database {
        path: db.to_path_buf(),
        message: if busy {
            format!("database busy/locked, try again later ({e})")
        } else {
            e.to_string()
        },
    }
}
