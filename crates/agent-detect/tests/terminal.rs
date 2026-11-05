//! Tests for interactive terminal launch recipes (`[agent.terminal]`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_detect::registry::{AgentDef, builtin_agents, parse_agents};
use agent_detect::terminal::{
    ApprovalMode, ContextDef, TerminalDef, TerminalLaunch, context_in_argv, supported_modes,
    terminal_launch,
};

const ALL_MODES: [ApprovalMode; 3] = [ApprovalMode::Auto, ApprovalMode::Ask, ApprovalMode::Plan];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn builtin(id: &str) -> &'static AgentDef {
    builtin_agents()
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("builtin agent {id}"))
}

fn launch(id: &str, mode: ApprovalMode) -> TerminalLaunch {
    terminal_launch(
        builtin(id),
        "/opt/bin/agent",
        Path::new("/work/dir"),
        "the context",
        mode,
    )
}

#[test]
fn every_builtin_terminal_table_is_valid() {
    for agent in builtin_agents() {
        let Some(terminal) = &agent.terminal else {
            continue;
        };
        for (mode, args) in &terminal.modes {
            assert!(
                args.iter().all(|a| !a.is_empty()),
                "{}: empty arg in mode {mode:?}",
                agent.id
            );
            for arg in args {
                let lower = arg.to_lowercase();
                assert!(
                    !lower.contains("bypass")
                        && !lower.contains("skip")
                        && !lower.contains("danger"),
                    "{}: mode {mode:?} uses a bypass flag {arg}",
                    agent.id
                );
            }
        }
        match &terminal.context {
            ContextDef::SystemPromptFlag { flag } | ContextDef::PromptFlag { flag } => {
                assert!(flag.starts_with('-'), "{}: bad context flag", agent.id)
            }
            ContextDef::ConfigFlag { flag, key } => {
                assert!(flag.starts_with('-'), "{}: bad config flag", agent.id);
                assert!(
                    !key.is_empty() && !key.starts_with('-'),
                    "{}: bad config key",
                    agent.id
                );
            }
            ContextDef::FirstArg | ContextDef::None => {}
        }
        if let Some(flag) = &terminal.cwd_flag {
            assert!(flag.starts_with('-'), "{}: bad cwd flag", agent.id);
        }
    }
    for id in ["claude", "codex", "opencode"] {
        assert!(builtin(id).terminal.is_some(), "{id} has a terminal recipe");
    }
}

#[test]
fn claude_launch_per_mode() {
    let expected = [
        (ApprovalMode::Auto, "acceptEdits"),
        (ApprovalMode::Ask, "manual"),
        (ApprovalMode::Plan, "plan"),
    ];
    for (mode, value) in expected {
        assert_eq!(
            launch("claude", mode),
            TerminalLaunch {
                program: "/opt/bin/agent".into(),
                args: strings(&[
                    "--permission-mode",
                    value,
                    "--append-system-prompt=the context",
                ]),
                cwd: PathBuf::from("/work/dir"),
                mode: Some(mode),
            }
        );
    }
}

#[test]
fn codex_launch_per_mode_passes_context_as_developer_instructions() {
    let expected = [
        (ApprovalMode::Auto, ["-a", "never", "-s", "workspace-write"]),
        (
            ApprovalMode::Ask,
            ["-a", "on-request", "-s", "workspace-write"],
        ),
        (ApprovalMode::Plan, ["-a", "on-request", "-s", "read-only"]),
    ];
    for (mode, mode_args) in expected {
        let mut args = strings(&["-C", "/work/dir"]);
        args.extend(strings(&mode_args));
        args.push("-c".into());
        args.push(r#"developer_instructions="the context""#.into());
        let got = launch("codex", mode);
        assert_eq!(
            got,
            TerminalLaunch {
                program: "/opt/bin/agent".into(),
                args,
                cwd: PathBuf::from("/work/dir"),
                mode: Some(mode),
            }
        );
        assert!(!got.args.iter().any(|a| a == "--"), "{:?}", got.args);
    }
}

#[test]
fn opencode_launch_supported_and_unsupported_modes() {
    assert_eq!(
        launch("opencode", ApprovalMode::Auto).args,
        strings(&["--auto", "--prompt=the context"])
    );
    assert_eq!(
        launch("opencode", ApprovalMode::Auto).mode,
        Some(ApprovalMode::Auto)
    );
    assert_eq!(
        launch("opencode", ApprovalMode::Ask).args,
        strings(&["--prompt=the context"])
    );
    assert_eq!(
        launch("opencode", ApprovalMode::Ask).mode,
        Some(ApprovalMode::Ask)
    );
    // Plan is not offered by the TUI: plain launch, no mode args.
    let plan = launch("opencode", ApprovalMode::Plan);
    assert_eq!(plan.args, strings(&["--prompt=the context"]));
    assert_eq!(plan.mode, None);
}

#[test]
fn cwd_flag_only_for_codex() {
    for agent in builtin_agents() {
        for mode in ALL_MODES {
            let got = launch(&agent.id, mode);
            let has = got.args.iter().any(|a| a == "-C");
            assert_eq!(has, agent.id == "codex", "{} {mode:?}", agent.id);
            assert_eq!(got.cwd, PathBuf::from("/work/dir"));
        }
    }
}

#[test]
fn supported_modes_follow_the_recipe() {
    assert_eq!(supported_modes(builtin("claude")), ALL_MODES.to_vec());
    assert_eq!(supported_modes(builtin("codex")), ALL_MODES.to_vec());
    assert_eq!(
        supported_modes(builtin("opencode")),
        vec![ApprovalMode::Auto, ApprovalMode::Ask]
    );
}

#[test]
fn builtin_recipes_put_context_in_argv() {
    for id in ["claude", "codex", "opencode"] {
        assert!(context_in_argv(builtin(id)), "{id}");
    }
}

fn custom_agents(terminal: &str) -> Vec<AgentDef> {
    parse_agents(&format!(
        r#"
[[agent]]
id = "mine"
name = "Mine"
bin = "mine"
config_dirs = ["~/.mine"]
version_args = ["--version"]
transport = "mine-transport"
{terminal}
"#
    ))
    .expect("valid agent list")
}

#[test]
fn agent_without_terminal_table_falls_back_to_plain_bin() {
    let agents = custom_agents("");
    let agent = &agents[0];
    assert!(!context_in_argv(agent));
    assert_eq!(supported_modes(agent), Vec::<ApprovalMode>::new());
    for mode in ALL_MODES {
        assert_eq!(
            terminal_launch(agent, "/bin/mine", Path::new("/w"), "ctx", mode),
            TerminalLaunch {
                program: "/bin/mine".into(),
                args: vec![],
                cwd: PathBuf::from("/w"),
                mode: None,
            }
        );
    }
}

#[test]
fn custom_terminal_table_round_trips_via_parse_agents() {
    let agents = custom_agents(
        r#"
[agent.terminal]
args = ["tui"]
cwd_flag = "--dir"

[agent.terminal.context]
kind = "first_arg"

[agent.terminal.modes]
plan = ["--read-only"]
"#,
    );
    let agent = &agents[0];
    assert_eq!(
        agent.terminal,
        Some(TerminalDef {
            args: strings(&["tui"]),
            cwd_flag: Some("--dir".into()),
            context: ContextDef::FirstArg,
            modes: BTreeMap::from([(ApprovalMode::Plan, strings(&["--read-only"]))]),
        })
    );
    assert_eq!(
        terminal_launch(agent, "mine", Path::new("/w"), "ctx", ApprovalMode::Plan).args,
        strings(&["tui", "--dir", "/w", "--read-only", "--", "ctx"])
    );
    let auto = terminal_launch(agent, "mine", Path::new("/w"), "ctx", ApprovalMode::Auto);
    assert_eq!(auto.args, strings(&["tui", "--dir", "/w", "--", "ctx"]));
    assert_eq!(auto.mode, None);
}

#[test]
fn context_kind_none_keeps_context_out_of_argv() {
    let agents = custom_agents(
        r#"
[agent.terminal]
context = { kind = "none" }
"#,
    );
    let agent = &agents[0];
    assert!(!context_in_argv(agent));
    let got = terminal_launch(agent, "mine", Path::new("/w"), "ctx", ApprovalMode::Ask);
    assert_eq!(got.args, Vec::<String>::new());
    assert_eq!(got.mode, None);
}

#[test]
fn prompt_flag_context_and_mode_serde_names() {
    let agents = custom_agents(
        r#"
[agent.terminal]
context = { kind = "prompt_flag", flag = "--message" }
modes = { auto = ["--yes"], ask = [] }
"#,
    );
    let terminal = agents[0].terminal.as_ref().expect("terminal");
    assert_eq!(
        terminal.context,
        ContextDef::PromptFlag {
            flag: "--message".into()
        }
    );
    assert_eq!(
        supported_modes(&agents[0]),
        vec![ApprovalMode::Auto, ApprovalMode::Ask]
    );
    assert_eq!(terminal.cwd_flag, None);
    assert_eq!(terminal.args, Vec::<String>::new());
}

#[test]
fn unknown_mode_name_is_rejected() {
    let err = parse_agents(
        r#"
[[agent]]
id = "mine"
name = "Mine"
bin = "mine"
config_dirs = ["~/.mine"]
version_args = ["--version"]
transport = "t"

[agent.terminal.modes]
yolo = ["--x"]
"#,
    )
    .expect_err("unknown mode must fail");
    assert!(!err.is_empty());
}

/// A context that looks like an option stays one argument the agent reads
/// as text: glued to long flags, after `--` when positional.
#[test]
fn hyphen_context_is_never_an_option() {
    let cwd = std::path::Path::new("/work/dir");
    let ctx = "--dangerously-skip-permissions";
    for id in ["claude", "opencode"] {
        let got = agent_detect::terminal::terminal_launch(
            builtin(id),
            "/opt/bin/agent",
            cwd,
            ctx,
            ApprovalMode::Ask,
        );
        assert!(!got.args.iter().any(|a| a == ctx), "{id}: {:?}", got.args);
        assert!(got.args.last().unwrap().ends_with(ctx), "{id}");
    }
    let codex = agent_detect::terminal::terminal_launch(
        builtin("codex"),
        "/opt/bin/agent",
        cwd,
        ctx,
        ApprovalMode::Ask,
    );
    assert!(!codex.args.iter().any(|a| a == ctx), "{:?}", codex.args);
    let n = codex.args.len();
    assert_eq!(codex.args[n - 2], "-c");
    assert_eq!(
        codex.args[n - 1],
        format!("developer_instructions=\"{ctx}\"")
    );
}

/// Parse a rendered `key=value` assignment the way Codex parses `-c`.
fn parse_assignment(assignment: &str, key: &str) -> String {
    let table: toml::Table = toml::from_str(assignment)
        .unwrap_or_else(|e| panic!("assignment {assignment:?} is not TOML: {e}"));
    assert_eq!(table.len(), 1, "{assignment:?}");
    table
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no string {key} in {assignment:?}"))
        .to_string()
}

#[test]
fn config_flag_context_round_trips_through_toml() {
    let tricky = [
        "the context",
        "",
        "say \"hi\"",
        r"C:\path\to\file \n not a newline",
        "line one\nline two\n\nline four\n",
        "\nleading newline",
        "triple \"\"\" quotes \"\"\"",
        "''' literal ''' quotes '",
        "--dangerously-bypass-approvals-and-sandbox",
        "-c model=evil",
        "tabs\tand \u{7f} control \u{1} chars\r\n",
        "unicode: zażółć gęślą jaźń, 日本語, emoji \u{1F600}",
        "key = \"value\"\n[table]\nx = 1",
    ];
    for ctx in tricky {
        let got = terminal_launch(
            builtin("codex"),
            "codex",
            Path::new("/w"),
            ctx,
            ApprovalMode::Ask,
        );
        let n = got.args.len();
        assert_eq!(got.args[n - 2], "-c", "{ctx:?}");
        let assignment = &got.args[n - 1];
        assert!(
            assignment.starts_with("developer_instructions="),
            "{assignment:?}"
        );
        assert_eq!(
            parse_assignment(assignment, "developer_instructions"),
            ctx,
            "{assignment:?}"
        );
    }
}

#[test]
fn custom_config_flag_recipe_parses_and_launches() {
    let agents = custom_agents(
        r#"
[agent.terminal]
context = { kind = "config_flag", flag = "--config", key = "instructions" }
modes = { ask = [] }
"#,
    );
    let agent = &agents[0];
    assert_eq!(
        agent.terminal.as_ref().expect("terminal").context,
        ContextDef::ConfigFlag {
            flag: "--config".into(),
            key: "instructions".into()
        }
    );
    assert!(context_in_argv(agent));
    let got = terminal_launch(agent, "mine", Path::new("/w"), "a \"b\"", ApprovalMode::Ask);
    assert_eq!(
        got.args,
        strings(&["--config", r#"instructions="a \"b\"""#])
    );
    assert_eq!(got.mode, Some(ApprovalMode::Ask));
}
