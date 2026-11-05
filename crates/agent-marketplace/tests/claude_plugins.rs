//! Claude Code plugin adapter: argv plans, JSON parsing, skills inside
//! plugins, and outcomes confirmed by listings. Fake runner, temp dirs.

mod common;

use std::path::{Path, PathBuf};

use agent_marketplace::claude_plugins::{
    ClaudePlugins, parse_available, parse_marketplaces, parse_plugin_list, parse_result_line,
    plan_available, plan_install, plan_list, plan_marketplaces, plan_uninstall, plan_update,
};
use agent_marketplace::{
    AgentSelection, ChannelId, InstallRequest, Marketplace, MarketplaceError, RemoveRequest, Scope,
    SourceKind, SourceRef, UpdateRequest,
};
use common::{FakeRunner, err_out, out, rooted, skill_dir, strings};

const LIST: &str = include_str!("fixtures/claude_list.json");
const AVAILABLE: &str = include_str!("fixtures/claude_available.json");
const MARKETPLACES: &str = include_str!("fixtures/claude_marketplaces.json");

fn plugin(marketplace: &str, plugin: &str, skill: Option<&str>) -> SourceRef {
    SourceRef {
        kind: SourceKind::Plugin {
            marketplace: marketplace.to_string(),
            plugin: plugin.to_string(),
        },
        skill: skill.map(str::to_string),
    }
}

fn argv(rest: &[&str]) -> Vec<String> {
    strings(
        &["claude", "plugin"]
            .iter()
            .chain(rest)
            .copied()
            .collect::<Vec<_>>(),
    )
}

/// Plugin caches from the list fixture: doc-tools has two skills, single
/// is one root-level skill, team-lint (project) has one.
fn caches(r: &Path) {
    skill_dir(r, "plugins/cache/acme/doc-tools/1.2.3/skills/pdf", "# pdf");
    skill_dir(r, "plugins/cache/acme/doc-tools/1.2.3/skills/csv", "# csv");
    std::fs::create_dir_all(r.join("plugins/cache/acme/doc-tools/1.2.3/skills/.hidden")).unwrap();
    skill_dir(r, "plugins/cache/acme/single/0.1.0", "# single");
    skill_dir(
        r,
        "plugins/cache/acme/team-lint/2.0.0/skills/lint",
        "# lint",
    );
    skill_dir(r, ".claude/skills/local-dir", "# local");
}

fn result(outcome: &str, message: &str) -> String {
    format!(
        "Fetching marketplace…\n{{\"command\":\"install\",\"outcome\":\"{outcome}\",\"message\":\"{message}\"}}\n"
    )
}

// ---- plans ----

#[test]
fn read_plans() {
    assert_eq!(plan_list(&Scope::Global).args, strings(&["list", "--json"]));
    let project = plan_list(&Scope::Project(PathBuf::from("/work/app")));
    assert_eq!(project.cwd, Some(PathBuf::from("/work/app")));
    assert_eq!(
        plan_available().args,
        strings(&["list", "--json", "--available"])
    );
    assert_eq!(
        plan_marketplaces().args,
        strings(&["marketplace", "list", "--json"])
    );
}

#[test]
fn install_plan_qualifies_the_plugin_and_names_the_scope() {
    let req = InstallRequest {
        source: plugin("acme", "doc-tools", None),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    let plan = plan_install(&req).unwrap();
    assert_eq!(
        plan.args,
        strings(&["install", "doc-tools@acme", "--scope", "user", "--json"])
    );
    assert_eq!(plan.cwd, None);
    let project = plan_install(&InstallRequest {
        scope: Scope::Project(PathBuf::from("/work/app")),
        ..req.clone()
    })
    .unwrap();
    assert_eq!(
        project.args,
        strings(&["install", "doc-tools@acme", "--scope", "project", "--json"])
    );
    assert_eq!(project.cwd, Some(PathBuf::from("/work/app")));
    // Never auto-accepts a marketplace-declared install command.
    assert!(!plan.args.iter().any(|a| a == "-y" || a == "--yes"));
}

#[test]
fn install_plan_rejects_non_plugin_sources_and_bad_names() {
    let req = InstallRequest {
        source: SourceRef::github("o", "r", None),
        skills: vec![],
        scope: Scope::Global,
        agents: AgentSelection::Detected,
    };
    assert!(matches!(
        plan_install(&req),
        Err(MarketplaceError::Unsupported(_))
    ));
    for (m, p) in [("acme", "-x"), ("a@b", "p"), ("acme", "p q"), ("", "p")] {
        let bad = InstallRequest {
            source: plugin(m, p, None),
            ..req.clone()
        };
        assert!(
            matches!(plan_install(&bad), Err(MarketplaceError::Invalid(_))),
            "{m} {p}"
        );
    }
}

#[test]
fn uninstall_and_update_plans() {
    assert_eq!(
        plan_uninstall("doc-tools@acme", "user", None).unwrap().args,
        strings(&["uninstall", "doc-tools@acme", "--scope", "user", "--json"])
    );
    let local = plan_uninstall("lint", "local", Some(Path::new("/p"))).unwrap();
    assert_eq!(
        local.args,
        strings(&["uninstall", "lint", "--scope", "local", "--json"])
    );
    assert_eq!(local.cwd, Some(PathBuf::from("/p")));
    assert_eq!(
        plan_update("doc-tools@acme", "managed", None).unwrap().args,
        strings(&["update", "doc-tools@acme", "--scope", "managed", "--json"])
    );
    assert!(plan_uninstall("x", "everywhere", None).is_err());
    assert!(plan_update("-x", "user", None).is_err());
    assert!(plan_update("a@b@c", "user", None).is_err());
}

// ---- parsers ----

#[test]
fn parses_the_installed_list() {
    assert!(
        parse_plugin_list("{}").is_err(),
        "neither array nor wrapper"
    );
    let plugins = parse_plugin_list(&rooted(LIST, Path::new("/r"))).unwrap();
    assert_eq!(plugins.len(), 4);
    let doc = &plugins[0];
    assert_eq!(doc.id, "doc-tools@acme");
    assert_eq!(doc.plugin, "doc-tools");
    assert_eq!(doc.marketplace, "acme");
    assert_eq!(doc.version, "1.2.3");
    assert_eq!(doc.scope, "user");
    assert!(doc.enabled);
    assert_eq!(
        doc.root,
        Some(PathBuf::from("/r/plugins/cache/acme/doc-tools/1.2.3"))
    );
    assert_eq!(plugins[2].project_path, Some(PathBuf::from("/r/project")));
    // Also accepts the `--available` object form.
    let wrapped = format!(
        r#"{{"installed":{},"available":[]}}"#,
        rooted(LIST, Path::new("/r"))
    );
    assert_eq!(parse_plugin_list(&wrapped).unwrap().len(), 4);
}

#[test]
fn parses_available_plugins_and_marketplaces() {
    let available = parse_available(AVAILABLE).unwrap();
    assert_eq!(available.len(), 3);
    assert_eq!(available[0].plugin_id, "pdf-suite@acme");
    assert_eq!(
        available[0].source_path.as_deref(),
        Some("./plugins/pdf-suite")
    );
    assert_eq!(available[0].install_count, Some(1520));
    assert_eq!(
        available[1].source_path, None,
        "object sources are not paths"
    );
    assert_eq!(available[2].description, None);
    let markets = parse_marketplaces(&rooted(MARKETPLACES, Path::new("/r"))).unwrap();
    assert_eq!(markets.len(), 2);
    assert_eq!(markets[0].name, "acme");
    assert_eq!(
        markets[0].install_location,
        Some(PathBuf::from("/r/plugins/marketplaces/acme"))
    );
}

#[test]
fn result_line_is_the_last_json_line() {
    let line = parse_result_line(&result("failed", "only displayed")).unwrap();
    assert_eq!(line.outcome, "failed");
    assert_eq!(line.message, "only displayed");
    assert!(parse_result_line("Successfully installed").is_none());
}

// ---- adapter ----

#[test]
fn capabilities_reflect_what_is_automatable() {
    let runner = FakeRunner::default();
    let claude = ClaudePlugins::new(&runner);
    assert_eq!(claude.id(), ChannelId::ClaudePlugins);
    let caps = claude.capabilities();
    assert!(caps.search && caps.install && caps.remove && caps.update);
    assert!(!caps.check, "the CLI has no way to check without updating");
}

#[test]
fn list_exposes_each_skill_inside_installed_plugins() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    caches(r);
    let runner = FakeRunner::with(vec![out(0, &rooted(LIST, r))]);
    let claude = ClaudePlugins::new(&runner);
    let skills = claude.list(&Scope::Global).unwrap();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["csv", "pdf", "single"]);
    let pdf = &skills[1];
    assert_eq!(
        pdf.path,
        r.join("plugins/cache/acme/doc-tools/1.2.3/skills/pdf")
    );
    assert_eq!(pdf.agents, strings(&["claude"]));
    assert_eq!(pdf.channel, ChannelId::ClaudePlugins);
    assert_eq!(pdf.source, Some(plugin("acme", "doc-tools", Some("pdf"))));
    assert_eq!(runner.argvs(), vec![argv(&["list", "--json"])]);
}

#[test]
fn project_list_keeps_that_projects_plugins() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    caches(r);
    let runner = FakeRunner::with(vec![out(0, &rooted(LIST, r))]);
    let claude = ClaudePlugins::new(&runner);
    let skills = claude.list(&Scope::Project(r.join("project"))).unwrap();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["lint"]);
    assert_eq!(skills[0].scope, Scope::Project(r.join("project")));
}

#[test]
fn search_filters_available_plugins_and_lists_their_skills() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    skill_dir(
        r,
        "plugins/marketplaces/acme/plugins/pdf-suite/skills/read-pdf",
        "# r",
    );
    skill_dir(
        r,
        "plugins/marketplaces/acme/plugins/pdf-suite/skills/fill-forms",
        "# f",
    );
    let runner = FakeRunner::with(vec![out(0, AVAILABLE), out(0, &rooted(MARKETPLACES, r))]);
    let claude = ClaudePlugins::new(&runner);
    let results = claude.search("PDF").unwrap();
    let hits: Vec<(&str, &SourceRef)> = results
        .hits
        .iter()
        .map(|h| (h.name.as_str(), &h.source))
        .collect();
    assert_eq!(
        hits,
        vec![
            ("pdf-suite", &plugin("acme", "pdf-suite", None)),
            (
                "fill-forms",
                &plugin("acme", "pdf-suite", Some("fill-forms"))
            ),
            ("read-pdf", &plugin("acme", "pdf-suite", Some("read-pdf"))),
        ]
    );
    assert_eq!(results.hits[0].installs, Some(1520));
    assert_eq!(
        results.hits[0].description.as_deref(),
        Some("PDF reading and form filling")
    );
    assert_eq!(
        runner.argvs(),
        vec![
            argv(&["list", "--json", "--available"]),
            argv(&["marketplace", "list", "--json"]),
        ]
    );
}

#[test]
fn search_matches_skill_names_and_survives_a_failed_marketplace_listing() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    skill_dir(
        r,
        "plugins/marketplaces/acme/plugins/commit-helper/skills/write-commit",
        "# w",
    );
    let runner = FakeRunner::with(vec![out(0, AVAILABLE), out(0, &rooted(MARKETPLACES, r))]);
    let results = ClaudePlugins::new(&runner).search("write-commit").unwrap();
    let names: Vec<&str> = results.hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["write-commit"]);

    let runner = FakeRunner::with(vec![out(0, AVAILABLE), err_out(1, "", "boom")]);
    let results = ClaudePlugins::new(&runner).search("deploy").unwrap();
    let names: Vec<&str> = results.hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["deploy"]);
    assert_eq!(results.warnings.len(), 1);
}

#[test]
fn install_is_confirmed_by_the_listing() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    caches(r);
    let after = rooted(LIST, r);
    let before = after.replace("doc-tools@acme", "other-tools@acme");
    let runner = FakeRunner::with(vec![
        out(0, &before),
        out(
            0,
            &result("ok", "Successfully installed plugin: doc-tools@acme"),
        ),
        out(0, &after),
    ]);
    let outcome = ClaudePlugins::new(&runner)
        .install(&InstallRequest {
            source: plugin("acme", "doc-tools", None),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["doc-tools@acme"]));
    assert_eq!(
        runner.argvs(),
        vec![
            argv(&["list", "--json"]),
            argv(&["install", "doc-tools@acme", "--scope", "user", "--json"]),
            argv(&["list", "--json"]),
        ]
    );
}

#[test]
fn install_of_an_installed_plugin_changes_nothing() {
    let list = rooted(LIST, Path::new("/r"));
    let runner = FakeRunner::with(vec![
        out(0, &list),
        out(
            0,
            &result("ok", "Plugin is already installed (scope: user)"),
        ),
        out(0, &list),
    ]);
    let outcome = ClaudePlugins::new(&runner)
        .install(&InstallRequest {
            source: plugin("acme", "doc-tools", Some("pdf")),
            skills: strings(&["pdf"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("already installed"))
    );
    assert!(
        outcome.warnings.iter().any(|w| w.contains("whole plugin")),
        "skill selection is not possible: {:?}",
        outcome.warnings
    );
}

#[test]
fn refused_install_is_failed_with_the_result_message() {
    let list = rooted(LIST, Path::new("/r"));
    let runner = FakeRunner::with(vec![
        out(0, &list),
        err_out(
            1,
            &result("failed", "The install command was only displayed"),
            "",
        ),
        out(0, &list),
    ]);
    let err = ClaudePlugins::new(&runner)
        .install(&InstallRequest {
            source: plugin("acme", "risky", None),
            skills: vec![],
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap_err();
    match err {
        MarketplaceError::Failed { status, stderr } => {
            assert_eq!(status, Some(1));
            assert!(stderr.contains("only displayed"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn remove_uses_the_scope_the_plugin_is_installed_at() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let before = rooted(LIST, r);
    let after = before.replace("team-lint@acme", "gone@acme");
    let runner = FakeRunner::with(vec![
        out(0, &before),
        out(
            0,
            &result("ok", "Successfully uninstalled plugin: team-lint"),
        ),
        out(0, &after),
    ]);
    let project = r.join("project");
    let outcome = ClaudePlugins::new(&runner)
        .remove(&RemoveRequest {
            skills: strings(&["team-lint"]),
            scope: Scope::Project(project.clone()),
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["team-lint@acme"]));
    let calls = runner.calls();
    assert_eq!(
        calls[1].args,
        strings(&[
            "plugin",
            "uninstall",
            "team-lint@acme",
            "--scope",
            "project",
            "--json"
        ])
    );
    assert_eq!(calls[1].cwd, Some(project));
}

#[test]
fn remove_everything_is_unsupported() {
    let runner = FakeRunner::default();
    let err = ClaudePlugins::new(&runner)
        .remove(&RemoveRequest {
            skills: strings(&["*"]),
            scope: Scope::Global,
            agents: AgentSelection::All,
        })
        .unwrap_err();
    assert!(matches!(err, MarketplaceError::Unsupported(_)));
    assert!(runner.calls().is_empty());
}

#[test]
fn update_all_updates_every_plugin_in_scope_and_reports_version_changes() {
    let before = rooted(LIST, Path::new("/r"));
    let after = before
        .replace(r#""version": "1.2.3""#, r#""version": "1.3.0""#)
        .replace("doc-tools/1.2.3", "doc-tools/1.3.0");
    let runner = FakeRunner::with(vec![
        out(0, &before),
        out(
            0,
            "{\"command\":\"update\",\"outcome\":\"ok\",\"message\":\"updated\"}",
        ),
        out(
            0,
            "{\"command\":\"update\",\"outcome\":\"ok\",\"message\":\"single is already at the latest version (0.1.0).\"}",
        ),
        out(0, &after),
    ]);
    let outcome = ClaudePlugins::new(&runner)
        .update(&UpdateRequest {
            skills: vec![],
            scope: Scope::Global,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["doc-tools@acme"]));
    let argvs = runner.argvs();
    assert_eq!(
        argvs[1],
        argv(&["update", "doc-tools@acme", "--scope", "user", "--json"])
    );
    assert_eq!(
        argvs[2],
        argv(&["update", "single@acme", "--scope", "user", "--json"])
    );
    assert_eq!(argvs.len(), 4, "skills-dir and project plugins are skipped");
}

#[test]
fn check_is_unknown_without_running_anything() {
    let runner = FakeRunner::default();
    let infos = ClaudePlugins::new(&runner)
        .check(&[agent_marketplace::InstalledSkill {
            name: "pdf".to_string(),
            path: PathBuf::from("/x"),
            agents: vec![],
            scope: Scope::Global,
            channel: ChannelId::ClaudePlugins,
            source: Some(plugin("acme", "doc-tools", Some("pdf"))),
        }])
        .unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].available, None);
    assert!(runner.calls().is_empty());
}
