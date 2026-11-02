//! OpenCode `run --format json` parser (plan Task 5).

use std::path::Path;

use agent_run::event::AgentEvent;
use agent_run::transport::{Transport, parser};

fn fixture() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode/synthetic-turn.jsonl"),
    )
    .expect("fixture present")
}

fn parse_all(lines: &str) -> Vec<AgentEvent> {
    let mut p = parser(Transport::OpencodeRunJson, Path::new("/w"));
    lines.lines().flat_map(|l| p.parse_line(l)).collect()
}

#[test]
fn synthetic_turn() {
    let events = parse_all(&fixture());
    assert_eq!(
        events.first(),
        Some(&AgentEvent::SessionStarted {
            id: "ses_synthetic_0001".to_string()
        })
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Text { .. }))
            .count(),
        1
    );
    assert!(events.contains(&AgentEvent::ToolCall {
        name: "write".to_string(),
        target: Some("SKILL.md".to_string())
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnd { .. }))
    );
}

#[test]
fn session_id_reported_once() {
    let events = parse_all(&fixture());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1,
        "sessionID is stamped on every OpenCode event; we report it once"
    );

    // A second step_start (same session) must not repeat it either.
    let events = parse_all(
        "{\"type\":\"step_start\",\"sessionID\":\"ses_x\",\"part\":{}}\n\
         {\"type\":\"text\",\"sessionID\":\"ses_x\",\"part\":{\"text\":\"a\"}}\n\
         {\"type\":\"step_start\",\"sessionID\":\"ses_x\",\"part\":{}}\n",
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1
    );
}

#[test]
fn error_event() {
    let events = parse_all(
        "{\"type\":\"error\",\"sessionID\":\"ses_x\",\"error\":\"provider unavailable\"}\n",
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::SessionStarted {
                id: "ses_x".to_string()
            },
            AgentEvent::Error {
                message: "provider unavailable".to_string()
            },
        ]
    );

    // Auth failures surface as NeedsLogin, not a generic error (review focus 4).
    let events = parse_all(
        "{\"type\":\"error\",\"sessionID\":\"ses_x\",\"message\":\"missing authentication token\"}\n",
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::SessionStarted {
                id: "ses_x".to_string()
            },
            AgentEvent::NeedsLogin {
                message: "missing authentication token".to_string()
            },
        ]
    );
}

#[test]
fn structured_error_message() {
    let events = parse_all(
        r#"{"type":"error","error":{"name":"ProviderAuthError","data":{"message":"Please run opencode auth login"}}}"#,
    );
    assert_eq!(
        events,
        vec![AgentEvent::NeedsLogin {
            message: "Please run opencode auth login".to_string()
        }]
    );
}

#[test]
fn opencode_session_from_error_before_step() {
    let events = parse_all(
        "{\"type\":\"error\",\"sessionID\":\"ses_e\",\"error\":\"boom\"}\n\
         {\"type\":\"step_start\",\"sessionID\":\"ses_e\",\"part\":{}}\n",
    );
    assert_eq!(
        events.first(),
        Some(&AgentEvent::SessionStarted {
            id: "ses_e".to_string()
        }),
        "the first event carrying a sessionID reports it: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count(),
        1
    );
}

#[test]
fn opencode_tool_use_dedup() {
    let events = parse_all(
        "{\"type\":\"tool_use\",\"sessionID\":\"s\",\"part\":{\"tool\":\"write\",\"callID\":\"c1\",\"state\":{\"input\":{\"filePath\":\"SKILL.md\"}}}}\n\
         {\"type\":\"tool_use\",\"sessionID\":\"s\",\"part\":{\"tool\":\"write\",\"callID\":\"c1\",\"state\":{\"input\":{\"filePath\":\"SKILL.md\"}}}}\n\
         {\"type\":\"tool_use\",\"sessionID\":\"s\",\"part\":{\"tool\":\"write\",\"callID\":\"c2\",\"state\":{\"input\":{\"filePath\":\"other.md\"}}}}\n",
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::SessionStarted {
                id: "s".to_string()
            },
            AgentEvent::ToolCall {
                name: "write".to_string(),
                target: Some("SKILL.md".to_string())
            },
            AgentEvent::ToolCall {
                name: "write".to_string(),
                target: Some("other.md".to_string())
            },
        ]
    );
}

#[test]
fn opencode_string_input_parsed() {
    let events = parse_all(
        r#"{"type":"tool_use","sessionID":"s","part":{"tool":"edit","callID":"c9","state":{"input":"{\"filePath\":\"/w/SKILL.md\",\"content\":\"x\"}"}}}"#,
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::SessionStarted {
                id: "s".to_string()
            },
            AgentEvent::ToolCall {
                name: "edit".to_string(),
                target: Some("SKILL.md".to_string())
            },
        ]
    );
}
