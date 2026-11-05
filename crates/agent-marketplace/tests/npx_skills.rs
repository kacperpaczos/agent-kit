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
    AgentSelection, CancelToken, ChannelId, Command, CommandRunner, InstallRequest, InstalledSkill,
    Marketplace, MarketplaceError, Output, RemoveRequest, RunError, Scope, SourceKind, SourceRef,
    UpdateRequest,
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
    outputs: RefCell<VecDeque<Result<Output, RunError>>>,
}

impl FakeRunner {
    fn with(outputs: Vec<Result<Output, RunError>>) -> FakeRunner {
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
    fn run(&self, cmd: &Command) -> Result<Output, RunError> {
        self.calls.borrow_mut().push(cmd.clone());
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("unexpected extra command")
    }
}

fn out(status: i32, stdout: &str) -> Result<Output, RunError> {
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

/// A skill the lock file tracks (it has a GitHub source).
fn tracked(name: &str, scope: Scope) -> InstalledSkill {
    InstalledSkill {
        source: Some(SourceRef::github("acme", "tools", Some(name))),
        ..installed(name, scope)
    }
}

/// A tracked skill installed from a local path (upstream never checks it).
fn local(name: &str, scope: Scope) -> InstalledSkill {
    InstalledSkill {
        source: Some(SourceRef {
            kind: SourceKind::Local {
                path: PathBuf::from("/src").join(name),
            },
            skill: Some(name.to_string()),
        }),
        ..installed(name, scope)
    }
}

fn status(output: &str, code: i32) -> Output {
    Output {
        status: Some(code),
        stdout: output.to_string(),
        stderr: String::new(),
    }
}

fn availability(infos: &[agent_marketplace::UpdateInfo]) -> Vec<(&str, Option<bool>)> {
    infos
        .iter()
        .map(|i| (i.name.as_str(), i.available))
        .collect()
}

/// `list --json` output with `(name, agent display names)` rows.
fn list_json(rows: &[(&str, &[&str])]) -> Result<Output, RunError> {
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|(name, agents)| {
            serde_json::json!({
                "name": name,
                "path": format!("/home/u/.agents/skills/{name}"),
                "scope": "global",
                "agents": agents,
                "source": "acme/tools",
                "sourceUrl": "https://github.com/acme/tools.git",
                "sourceType": "github",
                "status": "tracked"
            })
        })
        .collect();
    out(0, &serde_json::Value::Array(rows).to_string())
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
    // Upstream remove rejects `--agent '*'`; omitting --agent targets every
    // known agent already.
    let plan = plan_remove(&all).unwrap();
    assert_eq!(plan.args, args(&["remove", "--skill", "*", "-y"]));
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
    let skills = [
        tracked("Frontend-Design", Scope::Global),
        local("my-local", Scope::Global),
        tracked("pdf", Scope::Global),
        installed("hand-made", Scope::Global),
    ];
    let infos = parse_check(&status(CHECK_UPDATES, 1), &skills);
    assert_eq!(
        availability(&infos),
        [
            ("Frontend-Design", Some(true)),
            ("my-local", None),
            // The comparison ran and reported updates: unlisted tracked = current.
            ("pdf", Some(false)),
            // Untracked skills are never checked upstream.
            ("hand-made", None),
        ]
    );
}

#[test]
fn check_is_unknown_without_positive_evidence() {
    let skills = [tracked("a", Scope::Global), installed("b", Scope::Global)];
    let ok = parse_check(&status(CHECK_UPTODATE, 0), &skills);
    assert_eq!(availability(&ok), [("a", Some(false)), ("b", None)]);
    for (stdout, code) in [
        ("", 0),
        ("Something new happened\n", 0),
        ("No global skills to check.\n", 0),
        ("3 project skill(s) already up to date\n", 0),
        // Exit 1 without any "has an update" line is a failure, not an update.
        ("✗ Failed to check skills from acme/tools\n", 1),
    ] {
        let infos = parse_check(&status(stdout, code), &skills);
        assert!(infos.iter().all(|i| i.available.is_none()), "{stdout:?}");
    }
    let killed = parse_check(&Output::default(), &skills);
    assert!(killed.iter().all(|i| i.available.is_none()));
    // A source that failed to check makes unlisted skills unknown.
    let partial = parse_check(
        &status("↑ a has an update\n✗ Failed to check skills from x/y\n", 1),
        &[tracked("a", Scope::Global), tracked("c", Scope::Global)],
    );
    assert_eq!(availability(&partial), [("a", Some(true)), ("c", None)]);
}

#[test]
fn check_recognises_project_skills_that_cannot_be_updated() {
    let proj = Scope::Project(project());
    let stdout = "Checking for skill updates…\n\nChecking 1 skill(s)…\n\n\
        ✓ All project skills are up to date\n\n\
        1 project skill(s) cannot be updated automatically (installed before skillPath tracking):\n  \
        • old-one\n    \
        To refresh: npx skills add acme/tools -y\n\n";
    let infos = parse_check(
        &status(stdout, 0),
        &[tracked("fresh", proj.clone()), tracked("old-one", proj)],
    );
    assert_eq!(
        availability(&infos),
        [("fresh", Some(false)), ("old-one", None)]
    );
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
    let runner = FakeRunner::with(vec![
        out(0, FIND_HITS),
        Err(RunError::Spawn("npx: not found".to_string())),
    ]);
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
        tracked("frontend-design", Scope::Global),
        tracked("local-one", Scope::Project(project())),
        local("my-local", Scope::Global),
    ];
    let infos = npx.check(&list).unwrap();
    assert_eq!(
        availability(&infos),
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
fn adapter_install_reports_only_confirmed_skills() {
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
        out(1, r#"[{"name":"x","status":"failed","error":"nope"}]"#),
    ]);
    let npx = NpxSkills::new(&runner);
    // Partial success: kept, with the failures and the exit code as warnings.
    let outcome = npx.install(&req).unwrap();
    assert_eq!(outcome.changed, args(&["frontend-design"]));
    assert_eq!(outcome.warnings.len(), 3);
    // Exit 0 without JSON: nothing confirmed.
    let fallback = npx.install(&req).unwrap();
    assert!(fallback.changed.is_empty());
    assert_eq!(fallback.warnings.len(), 1);
    assert!(matches!(
        npx.install(&req),
        Err(MarketplaceError::Failed { .. })
    ));
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

fn remove_req(skills: &[&str], agents: AgentSelection) -> RemoveRequest {
    RemoveRequest {
        skills: args(skills),
        scope: Scope::Global,
        agents,
    }
}

#[test]
fn adapter_remove_confirms_changes_with_list_before_and_after() {
    let runner = FakeRunner::with(vec![
        list_json(&[("a", &["Claude Code"]), ("b", &["Codex"])]),
        out(0, "Successfully removed 1 skill(s)\n"),
        list_json(&[("b", &["Codex"])]),
    ]);
    let npx = NpxSkills::new(&runner);
    let outcome = npx
        .remove(&remove_req(&["A"], AgentSelection::Detected))
        .unwrap();
    assert_eq!(outcome.changed, args(&["a"]));
    assert!(outcome.warnings.is_empty());
    let calls: Vec<Vec<String>> = runner.calls().into_iter().map(|c| c.args).collect();
    assert_eq!(
        calls,
        [
            args(&["--yes", "skills", "list", "-g", "--json"]),
            args(&["--yes", "skills", "remove", "A", "-g", "-y"]),
            args(&["--yes", "skills", "list", "-g", "--json"]),
        ]
    );
}

#[test]
fn adapter_remove_of_unmatched_name_changes_nothing() {
    let runner = FakeRunner::with(vec![
        list_json(&[("b", &["Codex"])]),
        out(0, "No skills found to remove.\n"),
        list_json(&[("b", &["Codex"])]),
    ]);
    let npx = NpxSkills::new(&runner);
    let outcome = npx
        .remove(&remove_req(&["zzz"], AgentSelection::Detected))
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert_eq!(outcome.warnings.len(), 1);
}

#[test]
fn adapter_remove_from_one_agent_and_everything() {
    let runner = FakeRunner::with(vec![
        list_json(&[("a", &["Claude Code", "Codex"])]),
        out(0, ""),
        list_json(&[("a", &["Codex"])]),
        list_json(&[("a", &["Codex"]), ("b", &["Codex"])]),
        out(0, ""),
        list_json(&[]),
    ]);
    let npx = NpxSkills::new(&runner);
    let one = npx
        .remove(&remove_req(&["a"], AgentSelection::Only(args(&["claude"]))))
        .unwrap();
    assert_eq!(one.changed, args(&["a"]));
    let all = npx
        .remove(&remove_req(&["*"], AgentSelection::All))
        .unwrap();
    assert_eq!(all.changed, args(&["a", "b"]));
}

#[test]
fn adapter_remove_keeps_partial_success_and_fails_without_any() {
    let failed = || {
        Ok(Output {
            status: Some(1),
            stdout: String::new(),
            stderr: "Failed to remove 1 skill(s)".to_string(),
        })
    };
    let runner = FakeRunner::with(vec![
        list_json(&[("a", &[]), ("b", &[])]),
        failed(),
        list_json(&[("b", &[])]),
        list_json(&[("b", &[])]),
        failed(),
        list_json(&[("b", &[])]),
    ]);
    let npx = NpxSkills::new(&runner);
    let partial = npx
        .remove(&remove_req(&["a", "b"], AgentSelection::Detected))
        .unwrap();
    assert_eq!(partial.changed, args(&["a"]));
    assert_eq!(partial.warnings.len(), 1);
    assert!(matches!(
        npx.remove(&remove_req(&["b"], AgentSelection::Detected)),
        Err(MarketplaceError::Failed { .. })
    ));
}

#[test]
fn adapter_remove_without_a_listing_confirms_nothing() {
    let runner = FakeRunner::with(vec![
        Ok(Output {
            status: Some(1),
            stdout: String::new(),
            stderr: "broken lock".to_string(),
        }),
        out(0, "Successfully removed 1 skill(s)\n"),
    ]);
    let npx = NpxSkills::new(&runner);
    let outcome = npx
        .remove(&remove_req(&["a"], AgentSelection::Detected))
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert_eq!(outcome.warnings.len(), 1);
    assert_eq!(runner.calls().len(), 2);
}

#[test]
fn custom_command_prefix_and_cancel_token() {
    let runner = FakeRunner::with(vec![out(0, "[]")]);
    let token = CancelToken::new();
    let npx = NpxSkills::with_command(&runner, "/opt/bin/skills", &[]).with_cancel(token.clone());
    npx.list(&Scope::Global).unwrap();
    let call = &runner.calls()[0];
    assert_eq!(call.program, "/opt/bin/skills");
    assert_eq!(call.args, args(&["list", "-g", "--json"]));
    token.cancel();
    assert!(
        call.cancel.is_cancelled(),
        "the command shares the adapter's token"
    );
    let _ = npx_skills::AGENT_NAMES;
}

#[test]
fn adapter_cancelled_run_is_reported() {
    let runner = FakeRunner::with(vec![Err(RunError::Cancelled)]);
    let npx = NpxSkills::new(&runner);
    assert!(matches!(
        npx.list(&Scope::Global),
        Err(MarketplaceError::Cancelled)
    ));
}

#[test]
fn adapter_update_reports_only_confirmed_skills() {
    let stdout = "\x1b[38;5;145mUpdating a, b…\x1b[0m\n\n  \x1b[38;5;145m✓\x1b[0m Updated a\n\n\x1b[38;5;145m✓ Updated 1 skill(s)\x1b[0m\n";
    let runner = FakeRunner::with(vec![
        out(0, stdout),
        out(0, "✓ All global skills are up to date\n"),
        out(0, "Updated\n"),
        out(1, "  ✓ Updated a\nFailed to update 1 skill(s)\n"),
        out(1, "Failed to update 1 skill(s)\n"),
    ]);
    let npx = NpxSkills::new(&runner);
    let req = UpdateRequest {
        skills: args(&["a", "b"]),
        scope: Scope::Global,
    };
    let outcome = npx.update(&req).unwrap();
    assert_eq!(outcome.changed, args(&["a"]));
    assert!(outcome.warnings.is_empty());
    // Already current: nothing changed, nothing to warn about.
    let current = npx.update(&req).unwrap();
    assert!(current.changed.is_empty());
    assert!(current.warnings.is_empty());
    // Unrecognised output: nothing confirmed.
    let unknown = npx.update(&req).unwrap();
    assert!(unknown.changed.is_empty());
    assert_eq!(unknown.warnings.len(), 1);
    // Partial success kept.
    let partial = npx.update(&req).unwrap();
    assert_eq!(partial.changed, args(&["a"]));
    assert_eq!(partial.warnings.len(), 1);
    assert!(matches!(
        npx.update(&req),
        Err(MarketplaceError::Failed { .. })
    ));
}
