mod common;

use std::path::PathBuf;

use agent_sessions::time::parse_rfc3339;
use agent_sessions::{
    AgentKind, Certainty, EventKind, Filter, Limits, Role, list_sessions, read_events,
};

const MAIN: &str = "11111111-2222-4333-8444-555555555555";

fn claude_only() -> Filter {
    Filter {
        agents: vec![AgentKind::Claude],
        ..Filter::default()
    }
}

#[test]
fn lists_main_session_and_subagent() {
    let (_dir, env) = common::home();
    let (sessions, warnings) = list_sessions(&env, &claude_only());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sessions.len(), 2);

    let sub = &sessions[0];
    assert_eq!(sub.id, "agent-a1b2c3");
    assert!(sub.sidechain);
    assert_eq!(sub.parent_id.as_deref(), Some(MAIN));

    let main = &sessions[1];
    assert_eq!(main.agent, AgentKind::Claude);
    assert_eq!(main.id, MAIN);
    assert_eq!(main.cwd, Some(PathBuf::from("/work/demo")));
    assert!(!main.sidechain);
    assert_eq!(main.started, parse_rfc3339("2026-01-02T10:00:00.000Z"));
    assert_eq!(main.ended, parse_rfc3339("2026-01-02T10:00:09.000Z"));
}

#[test]
fn sidechains_can_be_excluded() {
    let (_dir, env) = common::home();
    let filter = Filter {
        include_sidechains: false,
        ..claude_only()
    };
    let (sessions, _) = list_sessions(&env, &filter);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, MAIN);
}

#[test]
fn claude_config_dir_overrides_home() {
    let (dir, _) = common::home();
    let other = tempfile::tempdir().unwrap();
    let env = agent_sessions::FakeEnv::with_home(other.path()).var(
        "CLAUDE_CONFIG_DIR",
        dir.path().join(".claude").to_string_lossy(),
    );
    let (sessions, _) = list_sessions(&env, &claude_only());
    assert_eq!(sessions.len(), 2);
}

#[test]
fn reads_normalized_events() {
    let (_dir, env) = common::home();
    let (sessions, _) = list_sessions(&env, &claude_only());
    let main = sessions.iter().find(|s| s.id == MAIN).unwrap();
    let t = read_events(main, &Limits::default()).unwrap();
    assert!(!t.truncated);
    assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    assert!(t.warnings[0].message.contains("line 7"));

    let kinds: Vec<(Role, EventKind)> = t.events.iter().map(|e| (e.role, e.kind.clone())).collect();
    assert_eq!(
        kinds,
        vec![
            (Role::User, EventKind::Text("Please fill the form in report.pdf".into())),
            (Role::Assistant, EventKind::Text("I will use the PDF skill.".into())),
            (
                Role::Assistant,
                EventKind::SkillActivation {
                    name: "pdf".into(),
                    certainty: Certainty::Certain
                }
            ),
            (
                Role::Assistant,
                EventKind::ToolUse {
                    name: "Bash".into(),
                    input_summary:
                        r#"{"command":"curl -H 'Authorization: [REDACTED]' https://example.invalid"}"#
                            .into()
                }
            ),
            (
                Role::Assistant,
                EventKind::SkillActivation {
                    name: "legacy-skill".into(),
                    certainty: Certainty::Certain
                }
            ),
            (
                Role::Tool,
                EventKind::ToolResult {
                    is_error: true,
                    summary: "curl: (6) Could not resolve host".into()
                }
            ),
            (Role::Assistant, EventKind::Text("Done.".into())),
        ]
    );
    assert_eq!(t.events[0].ts, parse_rfc3339("2026-01-02T10:00:00.000Z"));
}

#[test]
fn redaction_can_be_turned_off() {
    let (_dir, env) = common::home();
    let (sessions, _) = list_sessions(&env, &claude_only());
    let main = sessions.iter().find(|s| s.id == MAIN).unwrap();
    let limits = Limits {
        redact: false,
        ..Limits::default()
    };
    let t = read_events(main, &limits).unwrap();
    assert!(t.events.iter().any(|e| matches!(
        &e.kind,
        EventKind::ToolUse { input_summary, .. } if input_summary.contains("abcdefghijklmnop123")
    )));
}

#[test]
fn limits_cap_texts_and_events() {
    let (_dir, env) = common::home();
    let (sessions, _) = list_sessions(&env, &claude_only());
    let main = sessions.iter().find(|s| s.id == MAIN).unwrap();

    let short = Limits {
        max_text: 10,
        ..Limits::default()
    };
    let t = read_events(main, &short).unwrap();
    assert_eq!(t.events[0].kind, EventKind::Text("Please …".into()));

    let few = Limits {
        max_events: 2,
        ..Limits::default()
    };
    let t = read_events(main, &few).unwrap();
    assert_eq!(t.events.len(), 2);
    assert!(t.truncated);

    let small_total = Limits {
        max_total: 40,
        ..Limits::default()
    };
    let t = read_events(main, &small_total).unwrap();
    assert_eq!(t.events.len(), 1);
    assert!(t.truncated);
}

#[test]
fn oversized_lines_are_skipped_with_a_warning() {
    let (_dir, env) = common::home();
    let (sessions, _) = list_sessions(&env, &claude_only());
    let main = sessions.iter().find(|s| s.id == MAIN).unwrap();
    let limits = Limits {
        max_line: 300,
        ..Limits::default()
    };
    let t = read_events(main, &limits).unwrap();
    assert!(t.warnings.iter().any(|w| w.message.contains("line limit")));
    assert!(
        t.events
            .iter()
            .any(|e| e.kind == EventKind::Text("Done.".into()))
    );
}

#[test]
fn project_and_since_filters() {
    let (_dir, env) = common::home();
    let inside = Filter {
        project: Some(PathBuf::from("/work")),
        ..claude_only()
    };
    assert_eq!(list_sessions(&env, &inside).0.len(), 2);
    let elsewhere = Filter {
        project: Some(PathBuf::from("/work/dem")),
        ..claude_only()
    };
    assert!(list_sessions(&env, &elsewhere).0.is_empty());

    let late = Filter {
        since: parse_rfc3339("2026-01-02T10:00:30.000Z"),
        ..claude_only()
    };
    let (sessions, _) = list_sessions(&env, &late);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "agent-a1b2c3");
}

#[test]
fn missing_home_lists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = agent_sessions::FakeEnv::with_home(dir.path());
    let (sessions, warnings) = list_sessions(&env, &Filter::default());
    assert!(sessions.is_empty());
    assert!(warnings.is_empty());
    let none = agent_sessions::FakeEnv::default();
    assert!(list_sessions(&none, &Filter::default()).0.is_empty());
}

#[test]
fn secrets_are_redacted_before_capping() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join(".claude/projects/-w");
    std::fs::create_dir_all(&project).unwrap();
    let secret = "z".repeat(9_000);
    let record = serde_json::json!({
        "type": "assistant", "sessionId": "s", "cwd": "/w",
        "timestamp": "2026-01-02T10:00:00Z",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": format!("config: password=\"{secret}\"")},
            {"type": "tool_use", "id": "t", "name": "Bash",
             "input": {"command": format!("export PASS && curl -d \"password={secret}\"")}}
        ]}
    });
    std::fs::write(project.join("s.jsonl"), format!("{record}\n")).unwrap();
    let env = agent_sessions::FakeEnv::with_home(dir.path());
    let (sessions, _) = list_sessions(&env, &claude_only());
    let t = read_events(&sessions[0], &Limits::default()).unwrap();
    assert_eq!(t.events.len(), 2);
    for e in &t.events {
        let text = format!("{:?}", e.kind);
        assert!(!text.contains("zzzz"), "{}", &text[..text.len().min(120)]);
        assert!(text.contains("[REDACTED]"));
    }
}
