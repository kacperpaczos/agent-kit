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
    let (found, warnings) = skill_activations(&env, &Filter::default());
    // Broken lines in the fixtures surface as warnings.
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    let names: Vec<(AgentKind, String, Certainty)> = found
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
