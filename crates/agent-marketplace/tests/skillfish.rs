//! skillfish adapter: argv plans, JSON parsing, honest outcomes over a fake
//! runner. Nothing is spawned; files live in temp dirs only.

mod common;

use std::path::{Path, PathBuf};

use agent_marketplace::skillfish::{
    Skillfish, parse_list, parse_search, parse_update, plan_check, plan_install, plan_list,
    plan_remove, plan_search, plan_update_all,
};
use agent_marketplace::{
    AgentSelection, ChannelId, InstallRequest, InstalledSkill, Marketplace, MarketplaceError,
    RemoveRequest, Scope, SourceKind, SourceRef, UpdateRequest,
};
use common::{FakeRunner, err_out, out, rooted, skill_dir, strings, write};

const SEARCH: &str = include_str!("fixtures/skillfish_search.json");
const LIST: &str = include_str!("fixtures/skillfish_list.json");
const ADD: &str = include_str!("fixtures/skillfish_add.json");
const REMOVE: &str = include_str!("fixtures/skillfish_remove.json");
const UPDATE_CHECK: &str = include_str!("fixtures/skillfish_update_check.json");
const UPDATE_APPLY: &str = include_str!("fixtures/skillfish_update_apply.json");

const PREFIX: [&str; 3] = ["npx", "--yes", "skillfish"];

fn argv(rest: &[&str]) -> Vec<String> {
    strings(&PREFIX.iter().chain(rest).copied().collect::<Vec<_>>())
}

fn manifest(dir: &Path, owner: &str, repo: &str, path: &str, source: Option<&str>) {
    let source = source.map_or(String::new(), |s| format!(r#","source":"{s}""#));
    write(
        &dir.join(".skillfish.json"),
        &format!(
            r#"{{"version":2,"name":"{}","owner":"{owner}","repo":"{repo}","path":"{path}","branch":"main","sha":"{}"{source}}}"#,
            dir.file_name().unwrap().to_str().unwrap(),
            "1".repeat(40)
        ),
    );
}

fn installed(name: &str, path: PathBuf, agent: &str) -> InstalledSkill {
    InstalledSkill {
        name: name.to_string(),
        path,
        agents: vec![agent.to_string()],
        scope: Scope::Global,
        channel: ChannelId::Skillfish,
        source: None,
    }
}

fn github(owner: &str, repo: &str, skill: Option<&str>) -> SourceRef {
    SourceRef::github(owner, repo, skill)
}

// ---- plans ----

#[test]
fn search_passes_the_query_as_one_argument_with_json_and_limit() {
    let plan = plan_search("  pdf   tools ").unwrap();
    assert_eq!(
        plan.args,
        strings(&["--json", "search", "pdf tools", "--limit", "20"])
    );
    assert_eq!(plan.cwd, None);
    assert!(plan_search("   ").is_err());
    assert!(plan_search("pdf;rm").is_err());
    assert!(plan_search("-x").is_err());
}

#[test]
fn list_restricts_location_and_runs_project_scope_in_the_project() {
    let global = plan_list(&Scope::Global);
    assert_eq!(global.args, strings(&["--json", "list", "--global"]));
    assert_eq!(global.cwd, None);
    let project = plan_list(&Scope::Project(PathBuf::from("/work/app")));
    assert_eq!(project.args, strings(&["--json", "list", "--project"]));
    assert_eq!(project.cwd, Some(PathBuf::from("/work/app")));
}

#[test]
fn check_is_update_json_without_yes() {
    let home = Path::new("/home/dev");
    let global = plan_check(&Scope::Global, home);
    assert_eq!(global.args, strings(&["--json", "update"]));
    // Global runs from HOME so no unrelated project is looked at.
    assert_eq!(global.cwd, Some(home.to_path_buf()));
    let project = plan_check(&Scope::Project(PathBuf::from("/work/app")), home);
    assert_eq!(project.cwd, Some(PathBuf::from("/work/app")));
    let apply = plan_update_all(&Scope::Global, home);
    assert_eq!(apply.args, strings(&["--json", "update", "--yes"]));
}

#[test]
fn install_plans_one_add_per_skill_with_display_agent_names() {
    let req = InstallRequest {
        source: github("anthropics", "skills", Some("pdf")),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Only(strings(&["claude", "codex", "opencode"])),
    };
    let plans = plan_install(&req).unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0].args,
        strings(&[
            "--json",
            "add",
            "anthropics/skills",
            "pdf",
            "--yes",
            "--global",
            "--agent",
            "Claude Code",
            "--agent",
            "Codex",
            "--agent",
            "OpenCode",
        ])
    );

    let two = InstallRequest {
        skills: strings(&["a", "b"]),
        scope: Scope::Project(PathBuf::from("/work/app")),
        agents: AgentSelection::All,
        ..req.clone()
    };
    let plans = plan_install(&two).unwrap();
    assert_eq!(
        plans.iter().map(|p| p.args.clone()).collect::<Vec<_>>(),
        vec![
            strings(&[
                "--json",
                "add",
                "anthropics/skills",
                "a",
                "--yes",
                "--project"
            ]),
            strings(&[
                "--json",
                "add",
                "anthropics/skills",
                "b",
                "--yes",
                "--project"
            ]),
        ]
    );
    assert!(
        plans
            .iter()
            .all(|p| p.cwd == Some(PathBuf::from("/work/app")))
    );

    let all = InstallRequest {
        source: github("o", "r", None),
        skills: strings(&["*"]),
        agents: AgentSelection::Detected,
        ..req.clone()
    };
    assert_eq!(
        plan_install(&all).unwrap()[0].args,
        strings(&["--json", "add", "o/r", "--all", "--yes", "--global"])
    );

    let bare = InstallRequest {
        source: github("o", "r", None),
        agents: AgentSelection::Detected,
        ..req.clone()
    };
    assert_eq!(
        plan_install(&bare).unwrap()[0].args,
        strings(&["--json", "add", "o/r", "--yes", "--global"])
    );
}

#[test]
fn install_rejects_non_github_sources_and_bad_values() {
    let base = InstallRequest {
        source: github("o", "r", None),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    for kind in [
        SourceKind::Url {
            url: "https://example.com/x.git".to_string(),
        },
        SourceKind::Local {
            path: PathBuf::from("/tmp/x"),
        },
        SourceKind::Plugin {
            marketplace: "m".to_string(),
            plugin: "p".to_string(),
        },
    ] {
        let req = InstallRequest {
            source: SourceRef { kind, skill: None },
            ..base.clone()
        };
        assert!(matches!(
            plan_install(&req),
            Err(MarketplaceError::Unsupported(_))
        ));
    }
    let bad = InstallRequest {
        skills: strings(&["--force"]),
        ..base.clone()
    };
    assert!(matches!(
        plan_install(&bad),
        Err(MarketplaceError::Invalid(_))
    ));
    let mixed = InstallRequest {
        skills: strings(&["*", "a"]),
        ..base.clone()
    };
    assert!(plan_install(&mixed).is_err());
}

#[test]
fn remove_plans_one_call_per_skill_and_agent() {
    let req = RemoveRequest {
        skills: strings(&["pdf", "csv"]),
        scope: Scope::Global,
        agents: AgentSelection::Only(strings(&["claude", "codex"])),
    };
    let plans: Vec<Vec<String>> = plan_remove(&req)
        .unwrap()
        .into_iter()
        .map(|p| p.args)
        .collect();
    assert_eq!(
        plans,
        vec![
            strings(&[
                "--json",
                "remove",
                "pdf",
                "--yes",
                "--global",
                "--agent",
                "Claude Code"
            ]),
            strings(&[
                "--json", "remove", "pdf", "--yes", "--global", "--agent", "Codex"
            ]),
            strings(&[
                "--json",
                "remove",
                "csv",
                "--yes",
                "--global",
                "--agent",
                "Claude Code"
            ]),
            strings(&[
                "--json", "remove", "csv", "--yes", "--global", "--agent", "Codex"
            ]),
        ]
    );
    let all = RemoveRequest {
        skills: strings(&["*"]),
        scope: Scope::Project(PathBuf::from("/p")),
        agents: AgentSelection::All,
    };
    let plans = plan_remove(&all).unwrap();
    assert_eq!(
        plans[0].args,
        strings(&["--json", "remove", "--all", "--yes", "--project"])
    );
    assert_eq!(plans[0].cwd, Some(PathBuf::from("/p")));
    assert!(
        plan_remove(&RemoveRequest {
            skills: vec![],
            ..all.clone()
        })
        .is_err()
    );
}

// ---- parsers ----

#[test]
fn parses_search_results() {
    let results = parse_search(SEARCH).unwrap();
    assert!(results.warnings.is_empty());
    assert_eq!(results.hits.len(), 2);
    let pdf = &results.hits[0];
    assert_eq!(pdf.name, "pdf");
    assert_eq!(pdf.source, github("anthropics", "skills", Some("pdf")));
    assert_eq!(
        pdf.description.as_deref(),
        Some("Extract text and tables from PDF files")
    );
    assert_eq!(pdf.installs, None, "stars are not installs");
    assert_eq!(
        pdf.url.as_deref(),
        Some("https://www.skill.fish/skill/anthropics-skills-pdf")
    );
    assert!(parse_search("not json").is_err());
}

#[test]
fn list_keeps_only_skills_with_a_skillfish_manifest() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let claude_pdf = skill_dir(r, ".claude/skills/pdf", "# pdf");
    manifest(&claude_pdf, "anthropics", "skills", "skills/pdf", None);
    let codex_pdf = skill_dir(r, ".codex/skills/pdf", "# pdf");
    manifest(&codex_pdf, "anthropics", "skills", "skills/pdf", None);
    skill_dir(r, ".claude/skills/handmade", "# mine");
    let forms = skill_dir(r, ".cursor/skills/pdf-forms", "# forms");
    manifest(&forms, "acme", "pdf-forms", ".", None);

    let skills = parse_list(&rooted(LIST, r), &Scope::Global).unwrap();
    let names: Vec<(&str, &Vec<String>)> = skills
        .iter()
        .map(|s| (s.name.as_str(), &s.agents))
        .collect();
    assert_eq!(
        names,
        vec![
            ("pdf", &strings(&["claude"])),
            ("pdf", &strings(&["codex"])),
            ("pdf-forms", &strings(&["Cursor"])),
        ]
    );
    assert_eq!(skills[0].path, claude_pdf);
    assert_eq!(skills[0].channel, ChannelId::Skillfish);
    assert_eq!(skills[0].scope, Scope::Global);
    assert_eq!(
        skills[0].source,
        Some(github("anthropics", "skills", Some("pdf")))
    );
    assert!(parse_list("{", &Scope::Global).is_err());
}

#[test]
fn parses_update_reports() {
    let report = parse_update(UPDATE_CHECK).unwrap();
    assert_eq!(report.outdated.len(), 1);
    assert_eq!(report.outdated[0].skill, "pdf");
    assert_eq!(report.outdated[0].source, "anthropics/skills");
    assert_eq!(report.errors, strings(&["broken: Repository not found"]));
    assert!(report.updated.is_empty());
    let applied = parse_update(UPDATE_APPLY).unwrap();
    assert_eq!(applied.updated.len(), 1);
}

// ---- adapter end to end ----

#[test]
fn capabilities_and_id() {
    let runner = FakeRunner::default();
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    assert_eq!(fish.id(), ChannelId::Skillfish);
    let caps = fish.capabilities();
    assert!(caps.search && caps.install && caps.remove && caps.update && caps.check);
}

#[test]
fn search_runs_npx_skillfish() {
    let runner = FakeRunner::with(vec![out(0, SEARCH)]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let results = fish.search("pdf").unwrap();
    assert_eq!(results.hits.len(), 2);
    assert_eq!(
        runner.argvs(),
        vec![argv(&["--json", "search", "pdf", "--limit", "20"])]
    );
}

#[test]
fn install_reports_installed_skills_and_skips_as_warnings() {
    let runner = FakeRunner::with(vec![out(0, ADD)]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .install(&InstallRequest {
            source: github("anthropics", "skills", Some("pdf")),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf"]));
    assert_eq!(outcome.warnings.len(), 1);
    assert!(outcome.warnings[0].contains("Already exists"));
}

#[test]
fn install_failure_without_installs_is_an_error_with_json_errors() {
    let failed = r#"{"success":false,"installed":[],"skipped":[],"errors":["Repository not found"],"exit_code":4}"#;
    let runner = FakeRunner::with(vec![err_out(4, failed, "")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let err = fish
        .install(&InstallRequest {
            source: github("o", "missing", None),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap_err();
    match err {
        MarketplaceError::Failed { status, stderr } => {
            assert_eq!(status, Some(4));
            assert!(stderr.contains("Repository not found"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn install_with_unparsable_output_confirms_nothing() {
    let runner = FakeRunner::with(vec![out(0, "Installed!")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .install(&InstallRequest {
            source: github("o", "r", Some("x")),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert_eq!(outcome.warnings.len(), 1);
}

#[test]
fn remove_reports_removed_skills() {
    let runner = FakeRunner::with(vec![out(0, REMOVE)]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .remove(&RemoveRequest {
            skills: strings(&["pdf"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf"]));
    assert_eq!(
        runner.argvs(),
        vec![argv(&["--json", "remove", "pdf", "--yes", "--global"])]
    );
}

#[test]
fn remove_not_found_is_failed() {
    let none =
        r#"{"success":false,"exit_code":4,"errors":["Skill \"pdf\" not found"],"removed":[]}"#;
    let runner = FakeRunner::with(vec![err_out(4, none, "")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let err = fish
        .remove(&RemoveRequest {
            skills: strings(&["pdf"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        MarketplaceError::Failed {
            status: Some(4),
            ..
        }
    ));
}

#[test]
fn check_maps_outdated_errors_and_manifest_controlled_skills() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let pdf = skill_dir(r, ".claude/skills/pdf", "# pdf");
    manifest(&pdf, "anthropics", "skills", "skills/pdf", None);
    let csv = skill_dir(r, ".claude/skills/csv", "# csv");
    manifest(&csv, "anthropics", "skills", "skills/csv", None);
    let broken = skill_dir(r, ".claude/skills/broken", "# b");
    manifest(&broken, "gone", "repo", ".", None);
    let pinned = skill_dir(r, ".claude/skills/pinned", "# p");
    manifest(&pinned, "team", "skills", ".", Some("manifest"));

    let runner = FakeRunner::with(vec![out(0, &rooted(UPDATE_CHECK, r))]);
    let fish = Skillfish::new(&runner, r.to_path_buf());
    let infos = fish
        .check(&[
            installed("pdf", pdf, "claude"),
            installed("csv", csv, "claude"),
            installed("broken", broken, "claude"),
            installed("pinned", pinned, "claude"),
        ])
        .unwrap();
    let got: Vec<(&str, Option<bool>)> = infos
        .iter()
        .map(|i| (i.name.as_str(), i.available))
        .collect();
    assert_eq!(
        got,
        vec![
            ("pdf", Some(true)),
            ("csv", Some(false)),
            ("broken", None),
            ("pinned", None),
        ]
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args, argv(&["--json", "update"])[1..].to_vec());
    assert_eq!(calls[0].cwd, Some(r.to_path_buf()));
}

#[test]
fn check_after_a_failed_run_is_unknown_except_confirmed_outdated() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let pdf = skill_dir(r, ".claude/skills/pdf", "# pdf");
    manifest(&pdf, "anthropics", "skills", "skills/pdf", None);
    let csv = skill_dir(r, ".claude/skills/csv", "# csv");
    manifest(&csv, "anthropics", "skills", "skills/csv", None);
    let limited = rooted(UPDATE_CHECK, r).replace(r#""exit_code": 0"#, r#""exit_code": 3"#);
    let runner = FakeRunner::with(vec![err_out(3, &limited, "")]);
    let fish = Skillfish::new(&runner, r.to_path_buf());
    let infos = fish
        .check(&[
            installed("pdf", pdf, "claude"),
            installed("csv", csv, "claude"),
        ])
        .unwrap();
    assert_eq!(infos[0].available, Some(true));
    assert_eq!(infos[1].available, None);
}

#[test]
fn update_everything_applies_with_yes_and_reports_updated() {
    let runner = FakeRunner::with(vec![out(0, UPDATE_APPLY)]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .update(&UpdateRequest {
            skills: vec![],
            scope: Scope::Global,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf"]));
    assert_eq!(runner.argvs(), vec![argv(&["--json", "update", "--yes"])]);
}

#[test]
fn update_selected_skills_reinstalls_only_those_that_are_outdated() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let pdf = skill_dir(r, ".claude/skills/pdf", "# pdf");
    manifest(&pdf, "anthropics", "skills", "skills/pdf", None);
    let reinstall = r#"{"success":true,"installed":[{"skill":"pdf","agent":"Claude Code","path":"x","location":"global"}],"skipped":[],"errors":[],"exit_code":0}"#;
    let runner = FakeRunner::with(vec![out(0, &rooted(UPDATE_CHECK, r)), out(0, reinstall)]);
    let fish = Skillfish::new(&runner, r.to_path_buf());
    let outcome = fish
        .update(&UpdateRequest {
            skills: strings(&["pdf", "csv"]),
            scope: Scope::Global,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf"]));
    let argvs = runner.argvs();
    assert_eq!(argvs.len(), 2, "check, then one reinstall");
    assert_eq!(argvs[0], argv(&["--json", "update"]));
    assert_eq!(
        argvs[1],
        argv(&[
            "--json",
            "add",
            "anthropics/skills",
            "--path",
            "skills/pdf",
            "--force",
            "--yes",
            "--global",
            "--agent",
            "Claude Code",
        ])
    );
}

#[test]
fn update_selected_with_nothing_outdated_runs_no_install() {
    let uptodate = r#"{"success":true,"exit_code":0,"errors":[],"outdated":[],"updated":[]}"#;
    let runner = FakeRunner::with(vec![out(0, uptodate)]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .update(&UpdateRequest {
            skills: strings(&["pdf"]),
            scope: Scope::Global,
        })
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert_eq!(runner.calls().len(), 1);
}

#[test]
fn list_runs_and_parses() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let pdf = skill_dir(r, ".claude/skills/pdf", "# pdf");
    manifest(&pdf, "anthropics", "skills", "skills/pdf", None);
    let runner = FakeRunner::with(vec![out(0, &rooted(LIST, r))]);
    let fish = Skillfish::new(&runner, r.to_path_buf());
    let skills = fish.list(&Scope::Global).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(runner.argvs(), vec![argv(&["--json", "list", "--global"])]);
}

// ---- review regressions ----

const UNRECOGNISED: &str = include_str!("fixtures/skillfish_update_unrecognised.json");

fn outdated_json(rows: &[(&str, &Path, &str)]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|(skill, path, location)| {
            format!(
                r#"{{"skill":"{skill}","agent":"Claude Code","path":"{}","location":"{location}","localSha":"a","remoteSha":"b","source":"anthropics/skills"}}"#,
                path.display()
            )
        })
        .collect();
    format!(
        r#"{{"success":true,"exit_code":0,"errors":[],"outdated":[{}],"updated":[]}}"#,
        rows.join(",")
    )
}

fn added(skill: &str) -> String {
    format!(
        r#"{{"success":true,"installed":[{{"skill":"{skill}","agent":"Claude Code","path":"x","location":"project"}}],"skipped":[],"errors":[],"exit_code":0}}"#
    )
}

#[test]
fn project_update_reinstalls_only_project_records_never_update_yes() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let global = skill_dir(home.path(), ".claude/skills/pdf", "# g");
    manifest(&global, "anthropics", "skills", "skills/pdf", None);
    let local = skill_dir(project.path(), ".claude/skills/csv", "# p");
    manifest(&local, "anthropics", "skills", "skills/csv", None);
    let check = outdated_json(&[("pdf", &global, "global"), ("csv", &local, "project")]);
    let runner = FakeRunner::with(vec![out(0, &check), out(0, &added("csv"))]);
    let fish = Skillfish::new(&runner, home.path().to_path_buf());
    let outcome = fish
        .update(&UpdateRequest {
            skills: vec![],
            scope: Scope::Project(project.path().to_path_buf()),
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["csv"]));
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].args[2..], strings(&["--json", "update"]));
    assert!(
        !calls
            .iter()
            .any(|c| c.args[2..] == strings(&["--json", "update", "--yes"]))
    );
    assert_eq!(
        calls[1].args[2..],
        strings(&[
            "--json",
            "add",
            "anthropics/skills",
            "--path",
            "skills/csv",
            "--force",
            "--yes",
            "--project",
            "--agent",
            "Claude Code",
        ])
    );
    assert_eq!(calls[1].cwd, Some(project.path().to_path_buf()));
}

#[test]
fn selective_update_of_a_ref_pinned_skill_is_unsupported_before_anything_runs() {
    let home = tempfile::tempdir().unwrap();
    let pdf = skill_dir(home.path(), ".claude/skills/pdf", "# pdf");
    write(
        &pdf.join(".skillfish.json"),
        r#"{"version":2,"name":"pdf","owner":"anthropics","repo":"skills","path":"skills/pdf","branch":"v1","sha":"a","ref":"v1"}"#,
    );
    let runner = FakeRunner::with(vec![out(0, &outdated_json(&[("pdf", &pdf, "global")]))]);
    let fish = Skillfish::new(&runner, home.path().to_path_buf());
    let err = fish
        .update(&UpdateRequest {
            skills: strings(&["pdf"]),
            scope: Scope::Global,
        })
        .unwrap_err();
    match err {
        MarketplaceError::Unsupported(msg) => assert!(msg.contains("v1"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(runner.calls().len(), 1, "only the check ran");
}

#[test]
fn unrecognised_update_json_is_not_up_to_date() {
    assert!(parse_update(UNRECOGNISED).is_err());
    let root = tempfile::tempdir().unwrap();
    let pdf = skill_dir(root.path(), ".claude/skills/pdf", "# pdf");
    manifest(&pdf, "anthropics", "skills", "skills/pdf", None);
    let runner = FakeRunner::with(vec![out(0, UNRECOGNISED)]);
    let fish = Skillfish::new(&runner, root.path().to_path_buf());
    let err = fish.check(&[installed("pdf", pdf, "claude")]).unwrap_err();
    assert!(matches!(err, MarketplaceError::Parse(_)), "{err:?}");
}

#[test]
fn unrecognised_add_json_confirms_nothing() {
    let runner = FakeRunner::with(vec![out(0, "{}")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .install(&InstallRequest {
            source: github("o", "r", Some("x")),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert!(outcome.warnings[0].contains("unrecognised"));
}

#[test]
fn a_later_failure_keeps_earlier_confirmed_installs_and_removals() {
    let failed = r#"{"success":false,"installed":[],"skipped":[],"errors":["Repository not found"],"exit_code":4}"#;
    let runner = FakeRunner::with(vec![out(0, &added("a")), err_out(4, failed, "")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .install(&InstallRequest {
            source: github("o", "r", None),
            skills: strings(&["a", "b"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["a"]));
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("Repository not found"))
    );

    let none =
        r#"{"success":false,"exit_code":4,"errors":["Skill \"csv\" not found"],"removed":[]}"#;
    let runner = FakeRunner::with(vec![out(0, REMOVE), err_out(4, none, "")]);
    let fish = Skillfish::new(&runner, PathBuf::from("/home/dev"));
    let outcome = fish
        .remove(&RemoveRequest {
            skills: strings(&["pdf", "csv"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf"]));
    assert!(outcome.warnings.iter().any(|w| w.contains("not found")));
}
