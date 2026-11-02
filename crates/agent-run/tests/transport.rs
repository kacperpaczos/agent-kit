//! Tests for transport invocations, registry names, and the builtin registry
//! wiring of the OpenCode transport.

use agent_detect::registry::builtin_agents;
use agent_run::transport::{Transport, TurnSpec, invocation, parser};
use std::path::Path;

fn spec(transport: Transport) -> TurnSpec {
    TurnSpec {
        transport,
        cwd: Path::new("/tmp/work").to_path_buf(),
        model: None,
        resume: None,
        message: "popraw opis".to_string(),
    }
}

#[test]
fn claude_invocation_new_session() {
    let inv = invocation(&spec(Transport::ClaudeStreamJson));
    assert_eq!(
        inv.args,
        vec![
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ]
    );
    let line = inv.stdin.trim_end();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["type"], "user");
    assert_eq!(v["message"]["content"][0]["text"], "popraw opis");
}

#[test]
fn claude_invocation_resume_with_model() {
    let mut s = spec(Transport::ClaudeStreamJson);
    s.model = Some("opus".to_string());
    s.resume = Some("abc123".to_string());
    let inv = invocation(&s);
    assert_eq!(
        inv.args,
        vec![
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
            "--model",
            "opus",
            "--resume",
            "abc123",
        ]
    );
}

#[test]
fn claude_model_default_is_omitted() {
    let mut s = spec(Transport::ClaudeStreamJson);
    s.model = Some("default".to_string());
    assert!(!invocation(&s).args.contains(&"--model".to_string()));
}

#[test]
fn codex_invocation_new_and_resume() {
    let inv = invocation(&spec(Transport::CodexExecJson));
    assert_eq!(
        inv.args,
        vec![
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--sandbox",
            "workspace-write",
            "-C",
            "/tmp/work",
        ]
    );
    assert_eq!(inv.stdin, "popraw opis");

    let mut s = spec(Transport::CodexExecJson);
    s.model = Some("gpt-5.3".to_string());
    s.resume = Some("tid-9".to_string());
    let inv = invocation(&s);
    assert_eq!(
        inv.args,
        vec![
            "exec",
            "resume",
            "--json",
            "--skip-git-repo-check",
            "-c",
            "sandbox_mode=\"workspace-write\"",
            "-m",
            "gpt-5.3",
            "tid-9",
        ]
    );
}

#[test]
fn opencode_invocation_always_has_dir() {
    let inv = invocation(&spec(Transport::OpencodeRunJson));
    assert_eq!(
        inv.args,
        vec![
            "run",
            "--format",
            "json",
            "--dangerously-skip-permissions",
            "--dir",
            "/tmp/work",
        ]
    );
    assert_eq!(inv.stdin, "popraw opis");

    let mut s = spec(Transport::OpencodeRunJson);
    s.resume = Some("s1".to_string());
    s.model = Some("gpt-5".to_string());
    let inv = invocation(&s);
    assert_eq!(
        inv.args,
        vec![
            "run",
            "--format",
            "json",
            "--dangerously-skip-permissions",
            "--dir",
            "/tmp/work",
            "-s",
            "s1",
            "-m",
            "gpt-5",
        ]
    );
}

#[test]
fn prompt_never_in_args() {
    for t in [
        Transport::ClaudeStreamJson,
        Transport::CodexExecJson,
        Transport::OpencodeRunJson,
    ] {
        let mut s = spec(t);
        s.message = "SEKRETNY PROMPT 42".to_string();
        let inv = invocation(&s);
        assert!(!inv.args.contains(&s.message), "{t:?} leaks prompt in argv");
        assert!(inv.stdin.contains("SEKRETNY PROMPT 42"));
    }
}

#[test]
fn from_registry_names() {
    assert_eq!(
        Transport::from_registry("claude-stream-json"),
        Some(Transport::ClaudeStreamJson)
    );
    assert_eq!(
        Transport::from_registry("codex-exec-json"),
        Some(Transport::CodexExecJson)
    );
    assert_eq!(
        Transport::from_registry("opencode-run-json"),
        Some(Transport::OpencodeRunJson)
    );
    assert_eq!(Transport::from_registry("acp"), None);
    assert_eq!(Transport::from_registry("nope"), None);
}

#[test]
fn parser_until_tasks_3_and_5_returns_no_events() {
    for t in [Transport::ClaudeStreamJson, Transport::OpencodeRunJson] {
        let mut p = parser(t, Path::new("/tmp/work"));
        assert!(p.parse_line("{\"anything\": true}").is_empty());
        assert!(p.parse_line("not json").is_empty());
    }
}

#[test]
fn agents_toml_opencode_transport() {
    let opencode = builtin_agents()
        .iter()
        .find(|a| a.id == "opencode")
        .unwrap();
    assert_eq!(
        Transport::from_registry(&opencode.transport),
        Some(Transport::OpencodeRunJson)
    );
}
