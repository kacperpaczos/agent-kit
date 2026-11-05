mod common;

use std::path::PathBuf;

use agent_sessions::time::parse_rfc3339;
use agent_sessions::{
    AgentKind, Certainty, EventKind, Filter, Limits, Role, list_sessions, read_events,
    skill_activations,
};

const MAIN: &str = "99999999-8888-4777-8666-555555555555";
const CHILD: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn codex_only() -> Filter {
    Filter {
        agents: vec![AgentKind::Codex],
        ..Filter::default()
    }
}

#[test]
fn lists_rollouts_from_session_meta() {
    let (_dir, env) = common::home();
    let (sessions, warnings) = list_sessions(&env, &codex_only());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sessions.len(), 2);

    let child = &sessions[0];
    assert_eq!(child.id, CHILD);
    assert!(child.sidechain);
    assert_eq!(child.parent_id.as_deref(), Some(MAIN));

    let main = &sessions[1];
    assert_eq!(main.agent, AgentKind::Codex);
    assert_eq!(main.id, MAIN);
    assert_eq!(main.cwd, Some(PathBuf::from("/work/other")));
    assert!(!main.sidechain);
    assert_eq!(main.started, parse_rfc3339("2026-01-02T11:00:00.000Z"));
    // The last line is broken; the last readable one wins.
    assert_eq!(main.ended, parse_rfc3339("2026-01-02T11:00:10.000Z"));
}

#[test]
fn codex_home_overrides_home() {
    let (dir, _) = common::home();
    let other = tempfile::tempdir().unwrap();
    let env = agent_sessions::FakeEnv::with_home(other.path())
        .var("CODEX_HOME", dir.path().join(".codex").to_string_lossy());
    assert_eq!(list_sessions(&env, &codex_only()).0.len(), 2);
}

#[test]
fn reads_response_items() {
    let (_dir, env) = common::home();
    let (sessions, _) = list_sessions(&env, &codex_only());
    let main = sessions.iter().find(|s| s.id == MAIN).unwrap();
    let t = read_events(main, &Limits::default()).unwrap();
    assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    assert!(t.warnings[0].message.contains("line 13"));

    let kinds: Vec<(Role, EventKind)> = t.events.iter().map(|e| (e.role, e.kind.clone())).collect();
    let shell_input =
        r#"{"command":["bash","-lc","cat /home/u/.codex/skills/changelog-writer/SKILL.md"]}"#;
    assert_eq!(
        kinds,
        vec![
            (Role::System, EventKind::Text("<permissions instructions>".into())),
            (Role::User, EventKind::Text("Summarise the changelog".into())),
            (
                Role::Assistant,
                EventKind::ToolUse {
                    name: "shell".into(),
                    input_summary: shell_input.into()
                }
            ),
            (
                Role::Assistant,
                EventKind::SkillActivation {
                    name: "changelog-writer".into(),
                    certainty: Certainty::Inferred
                }
            ),
            (
                Role::Tool,
                EventKind::ToolResult {
                    is_error: false,
                    summary: r#"{"output":"---\nname: changelog-writer\n---","metadata":{"exit_code":0,"duration_seconds":0.1}}"#.into()
                }
            ),
            (
                Role::Assistant,
                EventKind::ToolUse {
                    name: "apply_patch".into(),
                    input_summary: "*** Begin Patch\n*** Update File: /home/u/.codex/skills/other/SKILL.md\n*** End Patch".into()
                }
            ),
            (
                Role::Tool,
                EventKind::ToolResult {
                    is_error: true,
                    summary: "Exit code: 1\nWall time: 0.1 seconds\nOutput:\npatch failed".into()
                }
            ),
            (
                Role::Assistant,
                EventKind::Text("Here is the summary. token=[REDACTED]".into())
            ),
            (
                Role::Assistant,
                EventKind::ToolUse {
                    name: "local_shell".into(),
                    input_summary: "sed -n 1,40p skills/notes/SKILL.md".into()
                }
            ),
            (
                Role::Assistant,
                EventKind::SkillActivation {
                    name: "notes".into(),
                    certainty: Certainty::Inferred
                }
            ),
        ]
    );
}

#[test]
fn skill_activations_across_agents() {
    let (_dir, env) = common::home();
    let out = skill_activations(&env, &Filter::default(), &Limits::default());
    assert!(!out.truncated);
    // Broken lines in the fixtures surface as warnings.
    assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
    let names: Vec<(AgentKind, String, Certainty)> = out
        .found
        .iter()
        .map(|(info, e)| match &e.kind {
            EventKind::SkillActivation { name, certainty } => {
                (info.agent, name.clone(), *certainty)
            }
            other => panic!("not an activation: {other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (
                AgentKind::Codex,
                "changelog-writer".into(),
                Certainty::Inferred
            ),
            (AgentKind::Codex, "notes".into(), Certainty::Inferred),
            (AgentKind::Claude, "pdf".into(), Certainty::Certain),
            (AgentKind::Claude, "legacy-skill".into(), Certainty::Certain),
        ]
    );
}

#[test]
fn broken_session_meta_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join(".codex/sessions/2026/01/03");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join("rollout-x.jsonl"),
        "garbage\n{\"type\":\"turn_context\"}\n",
    )
    .unwrap();
    std::fs::write(day.join("notes.txt"), "ignored").unwrap();
    let env = agent_sessions::FakeEnv::with_home(dir.path());
    let (sessions, warnings) = list_sessions(&env, &codex_only());
    assert!(sessions.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("session_meta"));
}

/// A rollout whose tool calls are given as `(name, arguments)` function
/// calls.
fn rollout_with(calls: &[(&str, String)]) -> (tempfile::TempDir, agent_sessions::FakeEnv) {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join(".codex/sessions/2026/02/01");
    std::fs::create_dir_all(&day).unwrap();
    let mut text = String::from(
        r#"{"timestamp":"2026-02-01T09:00:00Z","type":"session_meta","payload":{"id":"t-1","timestamp":"2026-02-01T09:00:00Z","cwd":"/w","originator":"x","cli_version":"0","source":"cli"}}"#,
    );
    text.push('\n');
    for (name, args) in calls {
        let line = serde_json::json!({
            "timestamp": "2026-02-01T09:00:01Z",
            "type": "response_item",
            "payload": {"type": "function_call", "name": name, "arguments": args, "call_id": "c"}
        });
        text.push_str(&line.to_string());
        text.push('\n');
    }
    std::fs::write(day.join("rollout-2026-02-01T09-00-00-t-1.jsonl"), text).unwrap();
    let env = agent_sessions::FakeEnv::with_home(dir.path());
    (dir, env)
}

fn bash(script: &str) -> String {
    serde_json::json!({"command": ["bash", "-lc", script]}).to_string()
}

fn inferred(env: &agent_sessions::FakeEnv, limits: &Limits) -> (Vec<String>, bool) {
    let out = skill_activations(env, &codex_only(), limits);
    let names = out
        .found
        .iter()
        .map(|(_, e)| match &e.kind {
            EventKind::SkillActivation { name, certainty } => {
                assert_eq!(*certainty, Certainty::Inferred);
                name.clone()
            }
            other => panic!("not an activation: {other:?}"),
        })
        .collect();
    (names, out.truncated)
}

#[test]
fn writes_and_deletes_are_not_activations() {
    let (_dir, env) = rollout_with(&[
        ("shell", bash("touch /s/a/SKILL.md")),
        ("shell", bash("rm -rf /s/b/SKILL.md")),
        ("shell", bash("mv /s/c/SKILL.md /tmp/old.md")),
        ("shell", bash("cp /tmp/new.md /s/d/SKILL.md")),
        ("shell", bash("printf 'x' > /s/e/SKILL.md")),
        ("shell", bash("sed -i 's/a/b/' /s/f/SKILL.md")),
        (
            "exec_command",
            r#"{"cmd":"echo /s/g/SKILL.md"}"#.to_string(),
        ),
        (
            "write_file",
            r#"{"path":"/s/h/SKILL.md","content":"x"}"#.to_string(),
        ),
    ]);
    assert_eq!(inferred(&env, &Limits::default()), (vec![], false));
}

#[test]
fn reads_of_several_skills_are_activations() {
    let (_dir, env) = rollout_with(&[
        (
            "shell",
            bash("cat /s/alpha/SKILL.md /s/beta/SKILL.md && head -n 20 skills/gamma/SKILL.md"),
        ),
        (
            "exec_command",
            r#"{"cmd":"rg -n name /s/delta/SKILL.md"}"#.to_string(),
        ),
        ("read_file", r#"{"path":"/s/epsilon/SKILL.md"}"#.to_string()),
    ]);
    assert_eq!(
        inferred(&env, &Limits::default()).0,
        ["alpha", "beta", "gamma", "delta", "epsilon"]
    );
}

#[test]
fn repeated_activations_are_bounded() {
    let calls: Vec<(&str, String)> = (0..50)
        .map(|_| ("shell", bash("cat /s/loop/SKILL.md")))
        .collect();
    let (_dir, env) = rollout_with(&calls);
    let (names, truncated) = inferred(&env, &Limits::default());
    assert_eq!(names.len(), 50);
    assert!(!truncated);
    let limits = Limits {
        max_events: 7,
        ..Limits::default()
    };
    let (names, truncated) = inferred(&env, &limits);
    assert_eq!(names.len(), 7);
    assert!(truncated);
    let limits = Limits {
        max_total: 4 * 10,
        ..Limits::default()
    };
    let (names, truncated) = inferred(&env, &limits);
    assert_eq!(names.len(), 10);
    assert!(truncated);
}
