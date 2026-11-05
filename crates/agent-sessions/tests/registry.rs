//! The default locations agree with agent-detect's `transcripts` entries.

use std::path::PathBuf;

use agent_detect::registry::builtin_agents;
use agent_sessions::{FakeEnv, Roots};

fn template(id: &str) -> (String, String) {
    let agent = builtin_agents().iter().find(|a| a.id == id).unwrap();
    let t = agent.transcripts.as_ref().expect("transcripts entry");
    (t.path.clone(), t.format.clone())
}

/// Expand `"$VAR/rest || ~/fallback"` for the two cases tested here.
fn expand(path: &str, home: &str, var: Option<(&str, &str)>) -> PathBuf {
    let (first, fallback) = path.split_once(" || ").unwrap();
    match var {
        Some((name, value)) => PathBuf::from(first.replace(&format!("${name}"), value)),
        None => PathBuf::from(home).join(fallback.strip_prefix("~/").unwrap()),
    }
}

#[test]
fn roots_match_registry_templates() {
    let cases = [
        ("claude", "claude-jsonl", "CLAUDE_CONFIG_DIR"),
        ("codex", "codex-rollout", "CODEX_HOME"),
        ("opencode", "opencode-sqlite", "XDG_DATA_HOME"),
    ];
    let pick = |roots: &Roots, id: &str| match id {
        "claude" => roots.claude_projects.clone(),
        "codex" => roots.codex_sessions.clone(),
        _ => roots.opencode_db.clone(),
    };
    for (id, format, var) in cases {
        let (path, fmt) = template(id);
        assert_eq!(fmt, format);
        let home = Roots::from_env(&FakeEnv::with_home("/h"));
        assert_eq!(pick(&home, id), Some(expand(&path, "/h", None)), "{id}");
        let env = FakeEnv::with_home("/h").var(var, "/v");
        let overridden = Roots::from_env(&env);
        assert_eq!(
            pick(&overridden, id),
            Some(expand(&path, "/h", Some((var, "/v")))),
            "{id}"
        );
    }
}
