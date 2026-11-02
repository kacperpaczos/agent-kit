//! Codex `exec --json` stream parser (Task 4).

use std::path::Path;

use agent_run::event::AgentEvent;
use agent_run::parse::CodexParser;
use agent_run::transport::LineParser;
use agent_run::transport::{Transport, parser};

fn fixture() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/turn-exec-json.jsonl"),
    )
    .unwrap()
}

fn feed(parser: &mut dyn agent_run::transport::LineParser, text: &str) -> Vec<AgentEvent> {
    text.lines().flat_map(|l| parser.parse_line(l)).collect()
}

#[test]
fn recording_turn() {
    let mut p = parser(Transport::CodexExecJson, Path::new("/WORK"));
    let events = feed(p.as_mut(), &fixture());
    let starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
        .collect();
    assert_eq!(
        starts,
        vec![&AgentEvent::SessionStarted {
            id: "01a04341-7b7e-7750-aeb4-721b12024ac2".into()
        }]
    );
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: true }));
    // The "Skill descriptions were shortened" warning item must not surface
    // as an Error, and reasoning items are dropped.
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Error { .. })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Text { text } if text == "DONE"))
    );
}

#[test]
fn file_change_is_tool_call() {
    let mut p = CodexParser::new("/WORK".into());
    let events = feed(&mut p, &fixture());
    assert!(events.contains(&AgentEvent::ToolCall {
        name: "file_change".into(),
        target: Some("notes.md".into()),
    }));
    assert!(events.contains(&AgentEvent::ToolCall {
        name: "command_execution".into(),
        target: None,
    }));
}

#[test]
fn turn_failed_is_error_and_turn_end_false() {
    let mut p = CodexParser::new("/WORK".into());
    let events = feed(
        &mut p,
        concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n",
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"stream disconnected\"}}\n",
        ),
    );
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: false }));
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::Error { message } if message.contains("stream disconnected"))
    ));
}

#[test]
fn auth_failure_is_needs_login() {
    let mut p = CodexParser::new("/WORK".into());
    let events = p.parse_line(
        "{\"type\":\"item.completed\",\"item\":{\"id\":\"i\",\"type\":\"error\",\"message\":\"Login required: run codex login\"}}",
    );
    assert!(events.contains(&AgentEvent::NeedsLogin {
        message: "Login required: run codex login".into()
    }));

    let mut p = CodexParser::new("/WORK".into());
    let events = p.parse_line(
        "{\"type\":\"turn.failed\",\"error\":{\"message\":\"login required: token expired\"}}",
    );
    assert!(events.contains(&AgentEvent::NeedsLogin {
        message: "login required: token expired".into()
    }));
}

#[test]
fn codex_top_level_error_frame() {
    let mut p = CodexParser::new("/WORK".into());
    assert_eq!(
        p.parse_line(r#"{"type":"error","message":"Stream error: connection reset"}"#),
        vec![AgentEvent::Error {
            message: "Stream error: connection reset".into()
        }]
    );
    // Reconnect progress chatter is not an error.
    assert_eq!(
        p.parse_line(r#"{"type":"error","message":"Reconnecting to relay…"}"#),
        Vec::<AgentEvent>::new()
    );
}
