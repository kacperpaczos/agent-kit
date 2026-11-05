//! `npx skills` adapter: argv plans, validation, output parsing, and the
//! adapter end to end over a fake runner (nothing is ever spawned).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use agent_marketplace::npx_skills::{
    self, NpxSkills, agent_id_for_display, npx_agent_name, parse_add_json, parse_check, parse_find,
    parse_list_json, plan_check, plan_install, plan_list, plan_remove, plan_search, plan_update,
};
use agent_marketplace::{
    AgentSelection, ChannelId, Command, CommandRunner, InstallRequest, InstalledSkill, Marketplace,
    MarketplaceError, Output, RemoveRequest, Scope, SourceKind, SourceRef, UpdateRequest,
};

const LIST_GLOBAL: &str = include_str!("fixtures/list_global.json");
const FIND_HITS: &str = include_str!("fixtures/find_hits.txt");
const FIND_NONE: &str = include_str!("fixtures/find_none.txt");
const CHECK_UPDATES: &str = include_str!("fixtures/check_updates.txt");
const CHECK_UPTODATE: &str = include_str!("fixtures/check_uptodate.txt");
const ADD_JSON: &str = include_str!("fixtures/add_json.json");

/// Records every command and answers from a queue of scripted outputs.
#[derive(Default)]
struct FakeRunner {
    calls: RefCell<Vec<Command>>,
    outputs: RefCell<VecDeque<Result<Output, String>>>,
}

impl FakeRunner {
    fn with(outputs: Vec<Result<Output, String>>) -> FakeRunner {
        FakeRunner {
            calls: RefCell::default(),
            outputs: RefCell::new(outputs.into()),
        }
    }
    fn calls(&self) -> Vec<Command> {
        self.calls.borrow().clone()
    }
}

impl CommandRunner for &FakeRunner {
    fn run(&self, cmd: &Command) -> Result<Output, String> {
        self.calls.borrow_mut().push(cmd.clone());
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("unexpected extra command")
    }
}

fn out(status: i32, stdout: &str) -> Result<Output, String> {
    Ok(Output {
        status: Some(status),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn project() -> PathBuf {
    PathBuf::from("/work/proj")
}

fn installed(name: &str, scope: Scope) -> InstalledSkill {
    InstalledSkill {
        name: name.to_string(),
        path: PathBuf::from(format!("/x/{name}")),
        agents: vec![],
        scope,
        channel: ChannelId::NpxSkills,
        source: None,
    }
}

// ---------- agents mapping ----------

#[test]
fn agent_ids_map_to_npx_names_and_back_from_display_names() {
    assert_eq!(npx_agent_name("claude"), Some("claude-code"));
    assert_eq!(npx_agent_name("codex"), Some("codex"));
    assert_eq!(npx_agent_name("opencode"), Some("opencode"));
    assert_eq!(npx_agent_name("nope"), None);
    assert_eq!(agent_id_for_display("Claude Code"), Some("claude"));
    assert_eq!(agent_id_for_display("Codex"), Some("codex"));
    assert_eq!(agent_id_for_display("OpenCode"), Some("opencode"));
    assert_eq!(agent_id_for_display("Cursor"), None);
}

// ---------- argv plans ----------

#[test]
fn search_splits_the_query_into_plain_words() {
    let plan = plan_search("react  hooks").unwrap();
    assert_eq!(plan.args, args(&["find", "react", "hooks"]));
    assert_eq!(plan.cwd, None);
}

#[test]
fn search_rejects_empty_flag_like_and_metachar_queries() {
    for bad in ["", "   ", "--owner", "a;rm", "x $(y)", "a|b", "`id`"] {
        assert!(
            matches!(plan_search(bad), Err(MarketplaceError::Invalid(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn list_global_and_project() {
    let global = plan_list(&Scope::Global);
    assert_eq!(global.args, args(&["list", "-g", "--json"]));
    assert_eq!(global.cwd, None);
    let proj = plan_list(&Scope::Project(project()));
    assert_eq!(proj.args, args(&["list", "--json"]));
    assert_eq!(proj.cwd, Some(project()));
}

#[test]
fn install_github_skill_globally_for_mapped_agents() {
    let req = InstallRequest {
        source: SourceRef::github("vercel-labs", "agent-skills", Some("frontend-design")),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Only(args(&["claude", "codex", "cursor"])),
    };
    let plan = plan_install(&req).unwrap();
    assert_eq!(
        plan.args,
        args(&[
            "add",
            "vercel-labs/agent-skills",
            "-g",
            "-y",
            "--json",
            "--skill",
            "frontend-design",
            "--agent",
            "claude-code",
            "codex",
            "cursor",
        ])
    );
    assert_eq!(plan.cwd, None);
}

#[test]
fn install_multiple_skills_all_agents_in_project() {
    let req = InstallRequest {
        source: SourceRef::github("acme", "tools", None),
        skills: args(&["one", "two"]),
        scope: Scope::Project(project()),
        agents: AgentSelection::All,
    };
    let plan = plan_install(&req).unwrap();
    assert_eq!(
        plan.args,
        args(&[
            "add",
            "acme/tools",
            "-y",
            "--json",
            "--skill",
            "one",
            "two",
            "--agent",
            "*",
        ])
    );
    assert_eq!(plan.cwd, Some(project()));
}

#[test]
fn install_every_skill_with_detected_agents_from_url_and_local() {
    let url = InstallRequest {
        source: SourceRef {
            kind: SourceKind::Url {
                url: "https://gitlab.com/org/repo".to_string(),
            },
            skill: None,
        },
        skills: args(&["*"]),
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    assert_eq!(
        plan_install(&url).unwrap().args,
        args(&[
            "add",
            "https://gitlab.com/org/repo",
            "-g",
            "-y",
            "--json",
            "--skill",
            "*"
        ])
    );
    let local = InstallRequest {
        source: SourceRef {
            kind: SourceKind::Local {
                path: PathBuf::from("/home/u/src/my-skills"),
            },
            skill: None,
        },
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    assert_eq!(
        plan_install(&local).unwrap().args,
        args(&["add", "/home/u/src/my-skills", "-g", "-y", "--json"])
    );
}

#[test]
fn install_rejects_bad_values_and_plugin_sources() {
    let base = InstallRequest {
        source: SourceRef::github("acme", "tools", None),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    let bad_sources = [
        SourceRef::github("-acme", "tools", None),
        SourceRef::github("acme", "to ols", None),
        SourceRef::github("acme", "tools;x", None),
        SourceRef::github("acme", "a/b", None),
        SourceRef::github("acme", "tools", Some("--all")),
        SourceRef {
            kind: SourceKind::Url {
                url: "https://x.y/$(id)".to_string(),
            },
            skill: None,
        },
        SourceRef {
            kind: SourceKind::Local {
                path: PathBuf::from("-rf"),
            },
            skill: None,
        },
    ];
    for source in bad_sources {
        let req = InstallRequest {
            source: source.clone(),
            ..base.clone()
        };
        assert!(
            matches!(plan_install(&req), Err(MarketplaceError::Invalid(_))),
            "{source:?}"
        );
    }
    for skills in [args(&["-g"]), args(&["a b"]), args(&["x&y"]), args(&[""])] {
        let req = InstallRequest {
            skills: skills.clone(),
            ..base.clone()
        };
        assert!(
            matches!(plan_install(&req), Err(MarketplaceError::Invalid(_))),
            "{skills:?}"
        );
    }
    for agents in [
        AgentSelection::Only(vec![]),
        AgentSelection::Only(args(&["--global"])),
        AgentSelection::Only(args(&["claude code"])),
        AgentSelection::Only(args(&["*"])),
    ] {
        let req = InstallRequest {
            agents: agents.clone(),
            ..base.clone()
        };
        assert!(
            matches!(plan_install(&req), Err(MarketplaceError::Invalid(_))),
            "{agents:?}"
        );
    }
    let plugin = InstallRequest {
        source: SourceRef {
            kind: SourceKind::Plugin {
                marketplace: "m".to_string(),
                plugin: "p".to_string(),
            },
            skill: None,
        },
        ..base
    };
    assert!(matches!(
        plan_install(&plugin),
        Err(MarketplaceError::Unsupported(_))
    ));
}

#[test]
fn remove_puts_names_before_greedy_agent_flag() {
    let req = RemoveRequest {
        skills: args(&["frontend-design", "pdf"]),
        scope: Scope::Global,
        agents: AgentSelection::Only(args(&["claude"])),
    };
    assert_eq!(
        plan_remove(&req).unwrap().args,
        args(&[
            "remove",
            "frontend-design",
            "pdf",
            "-g",
            "-y",
            "--agent",
            "claude-code"
        ])
    );
    let all = RemoveRequest {
        skills: args(&["*"]),
        scope: Scope::Project(project()),
        agents: AgentSelection::All,
    };
    let plan = plan_remove(&all).unwrap();
    assert_eq!(
        plan.args,
        args(&["remove", "--skill", "*", "-y", "--agent", "*"])
    );
    assert_eq!(plan.cwd, Some(project()));
}

#[test]
fn remove_requires_valid_names() {
    for skills in [vec![], args(&["-a"]), args(&["a;b"]), args(&["x y"])] {
        let req = RemoveRequest {
            skills: skills.clone(),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        };
        assert!(
            matches!(plan_remove(&req), Err(MarketplaceError::Invalid(_))),
            "{skills:?}"
        );
    }
}

#[test]
fn update_named_or_all_per_scope() {
    let named = UpdateRequest {
        skills: args(&["a", "b"]),
        scope: Scope::Global,
    };
    assert_eq!(
        plan_update(&named).unwrap().args,
        args(&["update", "a", "b", "-g", "-y"])
    );
    let all = UpdateRequest {
        skills: vec![],
        scope: Scope::Project(project()),
    };
    let plan = plan_update(&all).unwrap();
    assert_eq!(plan.args, args(&["update", "-p", "-y"]));
    assert_eq!(plan.cwd, Some(project()));
    let bad = UpdateRequest {
        skills: args(&["--all"]),
        scope: Scope::Global,
    };
    assert!(matches!(
        plan_update(&bad),
        Err(MarketplaceError::Invalid(_))
    ));
}

#[test]
fn check_plan_per_scope() {
    assert_eq!(
        plan_check(&Scope::Global, &args(&["a", "b"])).unwrap().args,
        args(&["check", "a", "b", "-g"])
    );
    let plan = plan_check(&Scope::Project(project()), &[]).unwrap();
    assert_eq!(plan.args, args(&["check", "-p"]));
    assert_eq!(plan.cwd, Some(project()));
    assert!(plan_check(&Scope::Global, &args(&["-x"])).is_err());
}

// ---------- parsing ----------

#[test]
fn list_json_maps_sources_agents_and_skips_missing() {
    let skills = parse_list_json(LIST_GLOBAL, &Scope::Global).unwrap();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["frontend-design", "my-local", "hand-made", "docs-helper"]
    );
    let fd = &skills[0];
    assert_eq!(
        fd.path,
        PathBuf::from("/home/u/.agents/skills/frontend-design")
    );
    assert_eq!(fd.agents, args(&["claude", "codex", "opencode", "Cursor"]));
    assert_eq!(fd.scope, Scope::Global);
    assert_eq!(fd.channel, ChannelId::NpxSkills);
    assert_eq!(
        fd.source,
        Some(SourceRef::github(
            "vercel-labs",
            "agent-skills",
            Some("frontend-design")
        ))
    );
    assert_eq!(
        skills[1].source.as_ref().unwrap().kind,
        SourceKind::Local {
            path: PathBuf::from("/home/u/src/my-local")
        }
    );
    assert_eq!(skills[2].source, None);
    assert_eq!(
        skills[3].source.as_ref().unwrap().kind,
        SourceKind::Url {
            url: "https://gitlab.com/org/repo.git".to_string()
        }
    );
}

#[test]
fn list_json_project_scope_and_empty_and_garbage() {
    let proj = Scope::Project(project());
    let skills = parse_list_json(
        r#"[{"name":"x","path":"/work/proj/.agents/skills/x","scope":"project","agents":["Codex"],"source":null,"sourceUrl":null,"sourceType":null,"status":"untracked"}]"#,
        &proj,
    )
    .unwrap();
    assert_eq!(skills[0].scope, proj);
    assert!(parse_list_json("[]\n", &proj).unwrap().is_empty());
    assert!(parse_list_json("No project skills found.", &proj).is_err());
}

#[test]
fn find_text_output_parses_hits() {
    let res = parse_find(FIND_HITS);
    assert!(res.warnings.is_empty(), "{:?}", res.warnings);
    assert_eq!(res.hits.len(), 4);
    let first = &res.hits[0];
    assert_eq!(first.name, "frontend-design");
    assert_eq!(
        first.source,
        SourceRef::github("vercel-labs", "agent-skills", Some("frontend-design"))
    );
    assert_eq!(first.installs, Some(1200));
    assert_eq!(first.description, None);
    assert_eq!(
        first.url.as_deref(),
        Some("https://skills.sh/vercel-labs/agent-skills/frontend-design")
    );
    assert_eq!(res.hits[1].installs, Some(3_000_000));
    assert_eq!(res.hits[2].installs, Some(1));
    assert_eq!(res.hits[3].installs, None);
}

#[test]
fn find_no_results_and_unrecognised_output() {
    let none = parse_find(FIND_NONE);
    assert!(none.hits.is_empty());
    assert!(none.warnings.is_empty());
    let odd = parse_find("Something new happened\nwith a different layout\n");
    assert!(odd.hits.is_empty());
    assert_eq!(odd.warnings.len(), 1);
}

#[test]
fn check_output_marks_updates_unknowns_and_current() {
    let names = args(&["Frontend-Design", "my-local", "pdf"]);
    let infos = parse_check(
        &Output {
            status: Some(1),
            stdout: CHECK_UPDATES.to_string(),
            stderr: String::new(),
        },
        &names,
    );
    let got: Vec<(&str, Option<bool>)> = infos
        .iter()
        .map(|i| (i.name.as_str(), i.available))
        .collect();
    assert_eq!(
        got,
        [
            ("Frontend-Design", Some(true)),
            ("my-local", None),
            ("pdf", Some(false))
        ]
    );
}

#[test]
fn check_exit_codes() {
    let names = args(&["a", "b"]);
    let ok = parse_check(
        &Output {
            status: Some(0),
            stdout: CHECK_UPTODATE.to_string(),
            stderr: String::new(),
        },
        &names,
    );
    assert!(ok.iter().all(|i| i.available == Some(false)));
    // Exit 1 without any "has an update" line is a failure, not an update.
    let failed = parse_check(
        &Output {
            status: Some(1),
            stdout: "✗ Failed to check skills from acme/tools\n".to_string(),
            stderr: String::new(),
        },
        &names,
    );
    assert!(failed.iter().all(|i| i.available.is_none()));
    let killed = parse_check(&Output::default(), &names);
    assert!(killed.iter().all(|i| i.available.is_none()));
}

#[test]
fn add_json_reports_installed_and_warns_on_failed_or_skipped() {
    let outcome = parse_add_json(ADD_JSON).unwrap();
    assert_eq!(outcome.changed, args(&["frontend-design"]));
    assert_eq!(outcome.warnings.len(), 2);
    assert!(outcome.warnings[0].contains("skill-creator"));
    assert!(outcome.warnings[0].contains("Failed to create symlink"));
    assert!(outcome.warnings[1].contains("old-one"));
    assert!(parse_add_json("not json").is_none());
}

// ---------- adapter over a fake runner ----------

#[test]
fn adapter_runs_npx_yes_skills_with_planned_argv() {
    let runner = FakeRunner::with(vec![out(0, LIST_GLOBAL)]);
    let npx = NpxSkills::new(&runner);
    assert_eq!(npx.id(), ChannelId::NpxSkills);
    let caps = npx.capabilities();
    assert!(caps.search && caps.install && caps.remove && caps.update && caps.check);
    let skills = npx.list(&Scope::Global).unwrap();
    assert_eq!(skills.len(), 4);
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].program, "npx");
    assert_eq!(
        calls[0].args,
        args(&["--yes", "skills", "list", "-g", "--json"])
    );
    assert!(calls[0].timeout.as_secs() > 0);
}

#[test]
fn adapter_search_and_spawn_failure() {
    let runner = FakeRunner::with(vec![out(0, FIND_HITS), Err("npx: not found".to_string())]);
    let npx = NpxSkills::new(&runner);
    assert_eq!(npx.search("design").unwrap().hits.len(), 4);
    assert!(matches!(
        npx.search("design"),
        Err(MarketplaceError::Run(_))
    ));
}

#[test]
fn adapter_list_failure_is_an_error() {
    let runner = FakeRunner::with(vec![Ok(Output {
        status: Some(1),
        stdout: String::new(),
        stderr: "Invalid agents".to_string(),
    })]);
    let npx = NpxSkills::new(&runner);
    assert!(matches!(
        npx.list(&Scope::Global),
        Err(MarketplaceError::Failed {
            status: Some(1),
            ..
        })
    ));
}

#[test]
fn adapter_check_groups_by_scope_and_keeps_input_order() {
    let runner = FakeRunner::with(vec![out(1, CHECK_UPDATES), out(0, CHECK_UPTODATE)]);
    let npx = NpxSkills::new(&runner);
    let list = [
        installed("frontend-design", Scope::Global),
        installed("local-one", Scope::Project(project())),
        installed("my-local", Scope::Global),
    ];
    let infos = npx.check(&list).unwrap();
    let got: Vec<(&str, Option<bool>)> = infos
        .iter()
        .map(|i| (i.name.as_str(), i.available))
        .collect();
    assert_eq!(
        got,
        [
            ("frontend-design", Some(true)),
            ("local-one", Some(false)),
            ("my-local", None)
        ]
    );
    let calls = runner.calls();
    assert_eq!(
        calls[0].args,
        args(&[
            "--yes",
            "skills",
            "check",
            "frontend-design",
            "my-local",
            "-g"
        ])
    );
    assert_eq!(
        calls[1].args,
        args(&["--yes", "skills", "check", "local-one", "-p"])
    );
    assert_eq!(calls[1].cwd, Some(project()));
}

#[test]
fn adapter_install_parses_json_and_falls_back_on_text() {
    let req = InstallRequest {
        source: SourceRef::github("vercel-labs", "agent-skills", None),
        skills: args(&["frontend-design", "skill-creator", "old-one"]),
        scope: Scope::Global,
        agents: AgentSelection::All,
    };
    let runner = FakeRunner::with(vec![
        out(1, ADD_JSON),
        out(0, "Installed 1 skill\n"),
        Ok(Output {
            status: Some(1),
            stdout: String::new(),
            stderr: "boom".to_string(),
        }),
    ]);
    let npx = NpxSkills::new(&runner);
    let outcome = npx.install(&req).unwrap();
    assert_eq!(outcome.changed, args(&["frontend-design"]));
    let fallback = npx.install(&req).unwrap();
    assert_eq!(
        fallback.changed,
        args(&["frontend-design", "skill-creator", "old-one"])
    );
    assert_eq!(fallback.warnings.len(), 1);
    assert!(matches!(
        npx.install(&req),
        Err(MarketplaceError::Failed { .. })
    ));
}

#[test]
fn adapter_invalid_request_never_runs() {
    let runner = FakeRunner::default();
    let npx = NpxSkills::new(&runner);
    let req = RemoveRequest {
        skills: args(&["$(reboot)"]),
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    assert!(npx.remove(&req).is_err());
    assert!(runner.calls().is_empty());
}

#[test]
fn adapter_remove_and_update_report_requested_names() {
    let runner = FakeRunner::with(vec![out(0, "Removed\n"), out(0, "Updated\n")]);
    let npx = NpxSkills::new(&runner);
    let removed = npx
        .remove(&RemoveRequest {
            skills: args(&["a"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(removed.changed, args(&["a"]));
    let updated = npx
        .update(&UpdateRequest {
            skills: args(&["b"]),
            scope: Scope::Global,
        })
        .unwrap();
    assert_eq!(updated.changed, args(&["b"]));
}

#[test]
fn custom_command_prefix() {
    let runner = FakeRunner::with(vec![out(0, "[]")]);
    let npx = NpxSkills::with_command(&runner, "/opt/bin/skills", &[]);
    npx.list(&Scope::Global).unwrap();
    assert_eq!(runner.calls()[0].program, "/opt/bin/skills");
    assert_eq!(runner.calls()[0].args, args(&["list", "-g", "--json"]));
    let _ = npx_skills::AGENT_NAMES;
}

#[test]
fn adapter_update_reads_per_skill_lines() {
    let stdout = "\x1b[38;5;145mUpdating a, b…\x1b[0m\n\n  \x1b[38;5;145m✓\x1b[0m Updated a\n\n\x1b[38;5;145m✓ Updated 1 skill(s)\x1b[0m\n";
    let runner = FakeRunner::with(vec![
        out(0, stdout),
        out(0, "✓ All global skills are up to date\n"),
    ]);
    let npx = NpxSkills::new(&runner);
    let req = UpdateRequest {
        skills: args(&["a", "b"]),
        scope: Scope::Global,
    };
    let outcome = npx.update(&req).unwrap();
    assert_eq!(outcome.changed, args(&["a"]));
    assert!(outcome.warnings.is_empty());
    let all = npx
        .update(&UpdateRequest {
            skills: vec![],
            scope: Scope::Global,
        })
        .unwrap();
    assert!(all.changed.is_empty());
}
