#![cfg(feature = "opencode")]

use std::path::{Path, PathBuf};

use agent_sessions::time::from_millis;
use agent_sessions::{
    AgentKind, Certainty, EventKind, FakeEnv, Filter, Limits, Role, list_sessions, read_events,
};
use rusqlite::{Connection, params};

const T0: i64 = 1_767_348_000_000; // 2026-01-02T10:00:00Z

/// The tables of opencode's `packages/core/src/session/sql.ts`, reduced to
/// the columns the reader uses plus a few it must ignore.
fn create(db: &Path, with_parts: bool) -> Connection {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, parent_id text,
            slug text NOT NULL, directory text NOT NULL, title text, version text NOT NULL,
            cost real NOT NULL DEFAULT 0, time_created integer NOT NULL, time_updated integer NOT NULL);
         CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
            seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL,
            data text NOT NULL);",
    )
    .unwrap();
    if with_parts {
        conn.execute_batch(
            "CREATE TABLE session_part (message_id text NOT NULL, session_id text NOT NULL,
                ordinal integer NOT NULL, type text NOT NULL, call_id text,
                time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL,
                PRIMARY KEY (message_id, ordinal));",
        )
        .unwrap();
    }
    conn
}

fn session(
    conn: &Connection,
    id: &str,
    dir: &str,
    parent: Option<&str>,
    created: i64,
    updated: i64,
) {
    conn.execute(
        "INSERT INTO session_v2 (id, project_id, parent_id, slug, directory, title, version,
            time_created, time_updated) VALUES (?1, 'prj_1', ?2, 's', ?3, 'Demo', '2.1.4', ?4, ?5)",
        params![id, parent, dir, created, updated],
    )
    .unwrap();
}

fn message(conn: &Connection, session: &str, id: &str, seq: i64, kind: &str, data: &str) {
    conn.execute(
        "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![id, session, kind, seq, T0 + seq * 1000, data],
    )
    .unwrap();
}

fn env_with_db(with_parts: bool) -> (tempfile::TempDir, FakeEnv, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(".local/share/opencode/opencode.db");
    let conn = create(&db, with_parts);
    session(&conn, "ses_main", "/work/oc", None, T0, T0 + 60_000);
    session(
        &conn,
        "ses_child",
        "/work/oc",
        Some("ses_main"),
        T0 + 1000,
        T0 + 2000,
    );
    message(
        &conn,
        "ses_main",
        "msg_1",
        1,
        "user",
        r#"{"text":"Review my diff","time":{"created":0}}"#,
    );
    message(
        &conn,
        "ses_main",
        "msg_2",
        2,
        "skill",
        r#"{"skill":"skl_1","name":"code-review","text":"…"}"#,
    );
    message(
        &conn,
        "ses_main",
        "msg_3",
        3,
        "model-switched",
        r#"{"model":{"id":"m","providerID":"p"}}"#,
    );
    message(&conn, "ses_main", "msg_4", 4, "user", "{not json");
    let assistant_content = r#"[
        {"type":"reasoning","text":"hidden"},
        {"type":"text","text":"Looking at it."},
        {"type":"tool","id":"call_1","name":"skill","state":{"status":"completed","input":{"name":"git-helper"},"content":[{"type":"text","text":"loaded"}]},"time":{"created":0}},
        {"type":"tool","id":"call_2","name":"bash","state":{"status":"error","input":{"command":"make test"},"error":{"message":"exit 2"}},"time":{"created":0}}
    ]"#;
    if with_parts {
        message(
            &conn,
            "ses_main",
            "msg_5",
            5,
            "assistant",
            r#"{"agent":"build","model":{"id":"m","providerID":"p"},"content":[],"time":{"created":0}}"#,
        );
        let items: Vec<serde_json::Value> = serde_json::from_str(assistant_content).unwrap();
        for (i, item) in items.iter().enumerate() {
            conn.execute(
                "INSERT INTO session_part (message_id, session_id, ordinal, type, time_created, time_updated, data)
                 VALUES ('msg_5', 'ses_main', ?1, ?2, 0, 0, ?3)",
                params![i as i64, item["type"].as_str().unwrap(), item.to_string()],
            )
            .unwrap();
        }
    } else {
        let data = format!(
            r#"{{"agent":"build","model":{{"id":"m","providerID":"p"}},"content":{assistant_content},"time":{{"created":0}}}}"#
        );
        message(&conn, "ses_main", "msg_5", 5, "assistant", &data);
    }
    message(
        &conn,
        "ses_main",
        "msg_6",
        6,
        "shell",
        r#"{"shellID":"sh_1","command":"ls","status":"completed","exit":0,"output":"a b"}"#,
    );
    drop(conn);
    let env = FakeEnv::with_home(dir.path());
    (dir, env, db)
}

fn oc_only() -> Filter {
    Filter {
        agents: vec![AgentKind::OpenCode],
        ..Filter::default()
    }
}

#[test]
fn lists_sessions_from_session_v2() {
    let (_dir, env, db) = env_with_db(false);
    let (sessions, warnings) = list_sessions(&env, &oc_only());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sessions.len(), 2);
    let main = &sessions[0];
    assert_eq!(main.id, "ses_main");
    assert_eq!(main.path, db);
    assert_eq!(main.cwd, Some(PathBuf::from("/work/oc")));
    assert_eq!(main.title.as_deref(), Some("Demo"));
    assert_eq!(main.started, from_millis(T0));
    assert_eq!(main.ended, from_millis(T0 + 60_000));
    assert!(!main.sidechain);
    let child = &sessions[1];
    assert!(child.sidechain);
    assert_eq!(child.parent_id.as_deref(), Some("ses_main"));
}

#[test]
fn xdg_data_home_overrides_home() {
    let (dir, _, _) = env_with_db(false);
    let other = tempfile::tempdir().unwrap();
    let env = FakeEnv::with_home(other.path()).var(
        "XDG_DATA_HOME",
        dir.path().join(".local/share").to_string_lossy(),
    );
    assert_eq!(list_sessions(&env, &oc_only()).0.len(), 2);
}

fn expected() -> Vec<(Role, EventKind)> {
    vec![
        (Role::User, EventKind::Text("Review my diff".into())),
        (
            Role::Assistant,
            EventKind::SkillActivation {
                name: "code-review".into(),
                certainty: Certainty::Certain,
            },
        ),
        (Role::Assistant, EventKind::Text("Looking at it.".into())),
        (
            Role::Assistant,
            EventKind::SkillActivation {
                name: "git-helper".into(),
                certainty: Certainty::Certain,
            },
        ),
        (
            Role::Tool,
            EventKind::ToolResult {
                is_error: false,
                summary: "loaded".into(),
            },
        ),
        (
            Role::Assistant,
            EventKind::ToolUse {
                name: "bash".into(),
                input_summary: r#"{"command":"make test"}"#.into(),
            },
        ),
        (
            Role::Tool,
            EventKind::ToolResult {
                is_error: true,
                summary: "exit 2".into(),
            },
        ),
        (
            Role::User,
            EventKind::ToolUse {
                name: "shell".into(),
                input_summary: "ls".into(),
            },
        ),
        (
            Role::Tool,
            EventKind::ToolResult {
                is_error: false,
                summary: "a b".into(),
            },
        ),
    ]
}

#[test]
fn reads_messages_with_inline_content() {
    let (_dir, env, _) = env_with_db(false);
    let (sessions, _) = list_sessions(&env, &oc_only());
    let t = read_events(&sessions[0], &Limits::default()).unwrap();
    assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    assert!(t.warnings[0].message.contains("msg_4"));
    let kinds: Vec<_> = t.events.iter().map(|e| (e.role, e.kind.clone())).collect();
    assert_eq!(kinds, expected());
    assert_eq!(t.events[0].ts, from_millis(T0 + 1000));
}

#[test]
fn reads_assistant_content_from_session_part() {
    let (_dir, env, _) = env_with_db(true);
    let (sessions, _) = list_sessions(&env, &oc_only());
    let t = read_events(&sessions[0], &Limits::default()).unwrap();
    let kinds: Vec<_> = t.events.iter().map(|e| (e.role, e.kind.clone())).collect();
    assert_eq!(kinds, expected());
}

#[test]
fn reading_never_writes_the_database() {
    let (_dir, env, db) = env_with_db(false);
    let before = std::fs::read(&db).unwrap();
    let (sessions, _) = list_sessions(&env, &oc_only());
    read_events(&sessions[0], &Limits::default()).unwrap();
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn unknown_schema_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(".local/share/opencode/opencode.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE session (id text PRIMARY KEY);")
        .unwrap();
    drop(conn);
    let env = FakeEnv::with_home(dir.path());
    let (sessions, warnings) = list_sessions(&env, &oc_only());
    assert!(sessions.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].message.contains("legacy layout"),
        "{warnings:?}"
    );
}

#[test]
fn not_a_database_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(".local/share/opencode/opencode.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(
        &db,
        "definitely not sqlite, just some text padding it out a bit more",
    )
    .unwrap();
    let env = FakeEnv::with_home(dir.path());
    let (sessions, warnings) = list_sessions(&env, &oc_only());
    assert!(sessions.is_empty());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}
