//! Tests for the Claude stream-json line parser (Task 3).

use std::path::{Path, PathBuf};

use agent_run::event::AgentEvent;
use agent_run::parse::claude::ClaudeParser;
use agent_run::transport::{LineParser, Transport, parser};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name),
    )
    .expect("fixture present")
}

fn feed_all(parser: &mut dyn LineParser, recording: &str) -> Vec<AgentEvent> {
    recording
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(|l| parser.parse_line(l))
        .collect()
}

#[test]
fn recording_single_turn() {
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    let events = feed_all(&mut p, &fixture("claude-2.1.259-no-partial-messages.jsonl"));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1,
        "exactly one SessionStarted: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Text { text } if !text.is_empty())),
        "at least one non-empty Text: {events:?}"
    );
    assert_eq!(*events.last().unwrap(), AgentEvent::TurnEnd { ok: true });
}

#[test]
fn recording_two_turns() {
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    let events = feed_all(
        &mut p,
        &fixture("claude-2.1.259-no-partial-two-turns.jsonl"),
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .count(),
        2,
        "two turns in the recording: {events:?}"
    );
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, AgentEvent::TurnEnd { ok: false }))
    );
}

#[test]
fn tool_use_maps_to_tool_call() {
    let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/w/SKILL.md","old_string":"a","new_string":"b"}}]}}"#;
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    assert_eq!(
        p.parse_line(line),
        vec![AgentEvent::ToolCall {
            name: "Edit".into(),
            target: Some("SKILL.md".into())
        }]
    );
}

#[test]
fn parser_factory_selects_claude() {
    let mut p = parser(Transport::ClaudeStreamJson, Path::new("/w"));
    let line = r#"{"type":"system","subtype":"init","session_id":"abc"}"#;
    assert_eq!(
        p.parse_line(line),
        vec![AgentEvent::SessionStarted { id: "abc".into() }]
    );
}

#[test]
fn error_result_is_error_then_turn_end() {
    let line = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"Turn limit reached","session_id":"s"}"#;
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    assert_eq!(
        p.parse_line(line),
        vec![
            AgentEvent::Error {
                message: "Turn limit reached".into()
            },
            AgentEvent::TurnEnd { ok: false },
        ]
    );
}
#[test]
fn login_error_is_needs_login() {
    let line = r#"{"type":"result","is_error":true,"result":"Please run /login to authenticate","session_id":"s"}"#;
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    let events = p.parse_line(line);
    assert!(
        events.contains(&AgentEvent::NeedsLogin {
            message: "Please run /login to authenticate".into()
        }),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "{events:?}"
    );
    assert_eq!(*events.last().unwrap(), AgentEvent::TurnEnd { ok: false });
}

#[test]
fn claude_errors_array_message() {
    let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"","errors":["API overloaded","rate limited"],"session_id":"s"}"#;
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    assert_eq!(
        p.parse_line(line),
        vec![
            AgentEvent::Error {
                message: "API overloaded; rate limited".into()
            },
            AgentEvent::TurnEnd { ok: false },
        ]
    );
}

#[test]
fn claude_error_subtype_fallback() {
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    assert_eq!(
        p.parse_line(
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"","session_id":"s"}"#
        ),
        vec![
            AgentEvent::Error {
                message: "Claude run failed: error_max_turns".into()
            },
            AgentEvent::TurnEnd { ok: false },
        ]
    );
    // No subtype and nothing else: still a non-empty message.
    assert_eq!(
        p.parse_line(r#"{"type":"result","is_error":true,"session_id":"s"}"#),
        vec![
            AgentEvent::Error {
                message: "Claude run failed".into()
            },
            AgentEvent::TurnEnd { ok: false },
        ]
    );
}

#[test]
fn garbage_line_ignored() {
    let mut p = ClaudeParser::new(PathBuf::from("/w"));
    assert_eq!(p.parse_line("not json at all"), Vec::<AgentEvent>::new());
    assert_eq!(
        p.parse_line(r#"{"type":"mystery"}"#),
        Vec::<AgentEvent>::new()
    );
    assert_eq!(p.parse_line(""), Vec::<AgentEvent>::new());
}
