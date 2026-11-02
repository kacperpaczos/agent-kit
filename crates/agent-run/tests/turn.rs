//! Plan Task 7: running one agent turn end to end.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use agent_detect::host::{FakeHost, NativeHost};
use agent_run::event::AgentEvent;
use agent_run::transport::{Transport, TurnSpec};
use agent_run::turn::{Stop, TurnSummary, run_turn};

const CLAUDE_SESSION: &str = "c8bc1209-0202-4ee6-98b4-7c744ec02f8b";
const OPENCODE_SESSION: &str = "ses_synthetic_0001";

fn fixture_lines(rel: &str) -> Vec<String> {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel),
    )
    .expect("fixture present")
    .lines()
    .map(str::to_string)
    .collect()
}

fn spec(transport: Transport, cwd: &Path, message: &str) -> TurnSpec {
    TurnSpec {
        transport,
        cwd: cwd.to_path_buf(),
        model: None,
        resume: None,
        message: message.to_string(),
    }
}

/// Runs one turn against a `FakeHost` scripting `lines`/`stderr`/`status`
/// for `/bin/agent`.
fn scripted_turn(
    transport: Transport,
    lines: Vec<String>,
    stderr: &str,
    status: Option<i32>,
) -> (Vec<AgentEvent>, TurnSummary) {
    let host = FakeHost::new();
    host.script_stream(PathBuf::from("/bin/agent"), lines, stderr, status);
    let mut events = Vec::new();
    let summary = run_turn(
        &host,
        Path::new("/bin/agent"),
        &[],
        &spec(transport, Path::new("/copy/demo"), "popraw opis"),
        &Stop::new(),
        &mut |e| events.push(e),
    );
    (events, summary)
}

fn turn_end_count(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
        .count()
}

#[test]
fn turn_streams_events_and_returns_session() {
    let (events, summary) = scripted_turn(
        Transport::ClaudeStreamJson,
        fixture_lines("claude/claude-2.1.259-no-partial-messages.jsonl"),
        "",
        Some(0),
    );
    assert_eq!(
        events.first(),
        Some(&AgentEvent::SessionStarted {
            id: CLAUDE_SESSION.to_string()
        })
    );
    assert!(events.contains(&AgentEvent::TurnEnd { ok: true }));
    assert_eq!(summary.session_id.as_deref(), Some(CLAUDE_SESSION));
    assert!(summary.ok);
}

#[test]
fn eof_without_turn_end_reports_stderr() {
    let (events, summary) = scripted_turn(
        Transport::OpencodeRunJson,
        fixture_lines("opencode/synthetic-turn.jsonl"),
        "fatal: brak klucza API",
        Some(1),
    );
    // OpenCode never emits TurnEnd; a non-zero exit with stderr must end
    // the turn with an Error carrying the agent's stderr (Review Focus 3).
    assert_eq!(turn_end_count(&events), 1);
    let tail: Vec<&AgentEvent> = events.iter().rev().take(2).collect();
    assert_eq!(
        tail[1],
        &AgentEvent::Error {
            message: "fatal: brak klucza API".to_string()
        }
    );
    assert_eq!(tail[0], &AgentEvent::TurnEnd { ok: false });
    assert!(!summary.ok);
    assert_eq!(summary.session_id.as_deref(), Some(OPENCODE_SESSION));
}

#[test]
fn exactly_one_turn_end() {
    let (events, _) = scripted_turn(
        Transport::ClaudeStreamJson,
        fixture_lines("claude/claude-2.1.259-no-partial-two-turns.jsonl"),
        "",
        Some(0),
    );
    assert_eq!(turn_end_count(&events), 1, "events: {events:?}");
}

#[test]
fn clean_exit_without_turn_end_is_ok() {
    let (events, summary) = scripted_turn(
        Transport::OpencodeRunJson,
        fixture_lines("opencode/synthetic-turn.jsonl"),
        "",
        Some(0),
    );
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: true }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { .. } | AgentEvent::NeedsLogin { .. }))
    );
    assert!(summary.ok);
}

#[test]
fn missing_binary_is_error() {
    let (events, summary) = scripted_turn(Transport::ClaudeStreamJson, vec![], "", None);
    assert!(matches!(events[0], AgentEvent::Error { .. }));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: false }));
    assert_eq!(summary.session_id, None);
    assert!(!summary.ok);
}

#[test]
fn spawn_gets_cwd_and_stdin() {
    let host = FakeHost::new();
    host.script_stream(
        PathBuf::from("/bin/agent"),
        fixture_lines("opencode/synthetic-turn.jsonl"),
        "",
        Some(0),
    );
    let mut events = Vec::new();
    run_turn(
        &host,
        Path::new("/bin/agent"),
        &[],
        &spec(
            Transport::OpencodeRunJson,
            Path::new("/copy/demo"),
            "popraw opis",
        ),
        &Stop::new(),
        &mut |e| events.push(e),
    );
    let spawned = host.spawned();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].cwd, PathBuf::from("/copy/demo"));
    assert!(spawned[0].stdin.contains("popraw opis"));
}

#[test]
fn stop_kills_silent_agent() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent.sh");
    let mut file = std::fs::File::create(&agent).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(file, "exec sleep 30").unwrap();
    drop(file);
    #[allow(clippy::disallowed_methods)] // test fixture setup, not host work
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let stop = Stop::new();
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        flag.stop();
    });
    let mut events = Vec::new();
    let start = Instant::now();
    let summary = run_turn(
        &NativeHost,
        &agent,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        &spec(Transport::OpencodeRunJson, dir.path(), "popraw opis"),
        &stop,
        &mut |e| events.push(e),
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(events.contains(&AgentEvent::Error {
        message: "Zatrzymano".to_string()
    }));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: false }));
    assert!(!summary.ok);
}

/// Writes an executable `/bin/sh` agent script into `dir`.
fn write_agent(dir: &Path, body: &str) -> PathBuf {
    let agent = dir.join("agent.sh");
    std::fs::write(&agent, format!("#!/bin/sh\n{body}")).unwrap();
    #[allow(clippy::disallowed_methods)] // test fixture setup, not host work
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    agent
}

#[test]
fn stop_kills_grandchildren() {
    let dir = tempfile::tempdir().unwrap();
    // A grandchild agent (npm launcher shape): the script's own children
    // must die with it, not keep the pipes open.
    let agent = write_agent(
        dir.path(),
        "sleep 30 &\necho $! > grandchild.pid\nsleep 30\n",
    );
    let stop = Stop::new();
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        flag.stop();
    });
    let mut events = Vec::new();
    let start = Instant::now();
    let summary = run_turn(
        &NativeHost,
        &agent,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        &spec(Transport::OpencodeRunJson, dir.path(), "popraw opis"),
        &stop,
        &mut |e| events.push(e),
    );
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(events.contains(&AgentEvent::Error {
        message: "Zatrzymano".to_string()
    }));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: false }));
    assert!(!summary.ok);
    // The grandchild really died with the group.
    let pid: i32 = std::fs::read_to_string(dir.path().join("grandchild.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Path::new("/proc").join(pid.to_string()).exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!Path::new("/proc").join(pid.to_string()).exists());
}

#[test]
fn turn_end_then_lingering_process_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let result_line = r#"{"type":"result","subtype":"success","is_error":false,"result":"gotowe","session_id":"s","total_cost_usd":0.0}"#;
    let agent = write_agent(dir.path(), &format!("echo '{result_line}'\nsleep 30\n"));
    let mut events = Vec::new();
    let start = Instant::now();
    let summary = run_turn(
        &NativeHost,
        &agent,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        &spec(Transport::ClaudeStreamJson, dir.path(), "popraw opis"),
        &Stop::new(),
        &mut |e| events.push(e),
    );
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "unexpected error event: {events:?}"
    );
    assert_eq!(turn_end_count(&events), 1);
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: true }));
    assert!(summary.ok);
}

#[test]
fn stderr_login_on_eof_is_needs_login() {
    let (events, summary) = scripted_turn(
        Transport::OpencodeRunJson,
        Vec::new(),
        "Please log in to continue",
        Some(1),
    );
    assert!(events.contains(&AgentEvent::NeedsLogin {
        message: "Please log in to continue".to_string()
    }));
    assert_eq!(events.last(), Some(&AgentEvent::TurnEnd { ok: false }));
    assert!(!summary.ok);
}

#[test]
fn is_login_message_cases() {
    use agent_run::event::is_login_message;
    for positive in [
        "You are not logged in",
        "visit /login to continue",
        "Please log in",
        "login required",
        "Unauthorized (401)",
        "401",
        "authentication failed",
        "run `auth login` first",
    ] {
        assert!(is_login_message(positive), "should match: {positive}");
    }
    for negative in ["the author of this change", "auth", "authority", "login"] {
        assert!(!is_login_message(negative), "should not match: {negative}");
    }
}

#[test]
fn missing_binary_message_names_the_program() {
    // FakeHost answers an unscripted program with NotFound.
    let host = FakeHost::new();
    let mut fresh = Vec::new();
    run_turn(
        &host,
        Path::new("/opt/nie-ma/agent"),
        &[],
        &spec(Transport::ClaudeStreamJson, Path::new("/copy/demo"), "x"),
        &Stop::new(),
        &mut |e| fresh.push(e),
    );
    assert_eq!(
        fresh[0],
        AgentEvent::Error {
            message: "Nie znaleziono programu agenta: /opt/nie-ma/agent".to_string()
        }
    );
}

#[test]
fn long_stderr_keeps_only_its_tail() {
    let stderr = format!("{}{}", "a".repeat(500), "b".repeat(2000));
    let (events, _) = scripted_turn(Transport::OpencodeRunJson, vec![], &stderr, Some(1));
    assert!(events.contains(&AgentEvent::Error {
        message: "b".repeat(2000)
    }));
}

#[test]
fn output_after_turn_end_is_drained_so_the_agent_can_exit() {
    let dir = tempfile::tempdir().unwrap();
    let result_line = r#"{"type":"result","subtype":"success","is_error":false,"result":"gotowe","session_id":"s","total_cost_usd":0.0}"#;
    // ~400 KB after the result: more than a pipe buffer holds.
    let agent = write_agent(
        dir.path(),
        &format!("echo '{result_line}'\nhead -c 400000 /dev/zero | tr '\\0' 'x'\necho\n"),
    );
    let start = Instant::now();
    let summary = run_turn(
        &NativeHost,
        &agent,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        &spec(Transport::ClaudeStreamJson, dir.path(), "popraw opis"),
        &Stop::new(),
        &mut |_| {},
    );
    assert!(summary.ok);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the agent exited on its own, it was not killed: {:?}",
        start.elapsed()
    );
}

#[test]
fn an_agent_finishing_up_after_turn_end_is_not_killed_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let result_line = r#"{"type":"result","subtype":"success","is_error":false,"result":"gotowe","session_id":"s","total_cost_usd":0.0}"#;
    let agent = write_agent(
        dir.path(),
        &format!("echo '{result_line}'\nsleep 1\ntouch finished\n"),
    );
    let summary = run_turn(
        &NativeHost,
        &agent,
        &[PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
        &spec(Transport::ClaudeStreamJson, dir.path(), "popraw opis"),
        &Stop::new(),
        &mut |_| {},
    );
    assert!(summary.ok);
    assert!(
        dir.path().join("finished").exists(),
        "the grace period after TurnEnd let the agent finish"
    );
}
