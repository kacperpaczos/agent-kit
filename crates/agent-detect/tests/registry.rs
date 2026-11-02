//! Tests for `[[agent]]` parsing and the builtin agent list.

use agent_detect::registry::{AgentDef, ModelsDef, SessionsDef, builtin_agents, parse_agents};

fn claude_agent() -> AgentDef {
    AgentDef {
        id: "claude".into(),
        name: "Claude Code".into(),
        bin: "claude".into(),
        fallback_bins: vec!["openclaude".into()],
        bin_env: Some("CLAUDE_BIN".into()),
        config_dirs: vec!["$CLAUDE_CONFIG_DIR || ~/.claude".into()],
        version_args: vec!["--version".into()],
        version_regex: None,
        auth_probe: Some(vec!["auth".into(), "status".into()]),
        transport: "claude-stream-json".into(),
        models: None,
        fallback_models: vec![
            "default".into(),
            "sonnet".into(),
            "opus".into(),
            "haiku".into(),
        ],
        sessions: Some(SessionsDef {
            path: "~/.claude.json".into(),
            parser: "claude-json".into(),
        }),
    }
}

fn opencode_agent() -> AgentDef {
    AgentDef {
        id: "opencode".into(),
        name: "OpenCode".into(),
        bin: "opencode".into(),
        fallback_bins: vec!["opencode-cli".into()],
        bin_env: Some("OPENCODE_BIN".into()),
        config_dirs: vec!["$XDG_CONFIG_HOME/opencode || ~/.config/opencode".into()],
        version_args: vec!["--version".into()],
        version_regex: None,
        auth_probe: None,
        transport: "opencode-run-json".into(),
        models: Some(ModelsDef {
            args: vec!["models".into(), "--verbose".into()],
            parser: "opencode-models".into(),
        }),
        fallback_models: vec!["default".into()],
        sessions: Some(SessionsDef {
            path: "$XDG_DATA_HOME/opencode/opencode.db || ~/.local/share/opencode/opencode.db"
                .into(),
            parser: "opencode-sqlite".into(),
        }),
    }
}

fn codex_agent() -> AgentDef {
    AgentDef {
        id: "codex".into(),
        name: "Codex".into(),
        bin: "codex".into(),
        fallback_bins: vec![],
        bin_env: Some("CODEX_BIN".into()),
        config_dirs: vec!["$CODEX_HOME || ~/.codex".into()],
        version_args: vec!["--version".into()],
        version_regex: None,
        auth_probe: None,
        transport: "codex-exec-json".into(),
        models: None,
        fallback_models: vec!["default".into()],
        sessions: Some(SessionsDef {
            path: "$CODEX_HOME/sessions || ~/.codex/sessions".into(),
            parser: "codex-jsonl".into(),
        }),
    }
}

#[test]
fn builtin_agents_match_the_bundled_toml() {
    assert_eq!(
        builtin_agents(),
        &[claude_agent(), opencode_agent(), codex_agent()]
    );
}

#[test]
fn parse_agents_reads_a_custom_list() {
    let agents = parse_agents(
        r#"
[[agent]]
id = "mine"
name = "Mine"
bin = "mine"
config_dirs = ["~/.mine"]
version_args = ["--version"]
transport = "mine-transport"
"#,
    )
    .expect("valid agent list");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "mine");
    assert_eq!(agents[0].fallback_bins, Vec::<String>::new());
    assert_eq!(agents[0].fallback_models, Vec::<String>::new());
    assert_eq!(agents[0].models, None);
    assert_eq!(agents[0].sessions, None);
}

#[test]
fn parse_agents_rejects_malformed_toml() {
    let err = parse_agents("[[agent]\nid = ").expect_err("malformed TOML must fail");
    assert!(!err.is_empty());
}

#[test]
fn parse_agents_rejects_a_missing_field() {
    let err = parse_agents(
        r#"
[[agent]]
id = "mine"
"#,
    )
    .expect_err("missing fields must fail");
    assert!(!err.is_empty());
}
