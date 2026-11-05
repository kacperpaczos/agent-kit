//! Codex plugin adapter (`codex plugin add|list|remove`): argv plans, JSON
//! parsing, capability limits. Fake runner, temp dirs.

mod common;

use std::path::{Path, PathBuf};

use agent_marketplace::codex_plugins::{
    CodexPlugins, parse_add, parse_codex_list, plan_add, plan_available, plan_list, plan_remove,
};
use agent_marketplace::{
    AgentSelection, ChannelId, InstallRequest, Marketplace, MarketplaceError, RemoveRequest, Scope,
    SourceKind, SourceRef, UpdateRequest,
};
use common::{FakeRunner, out, rooted, skill_dir, strings};

const LIST: &str = include_str!("fixtures/codex_list.json");
const ADD: &str = include_str!("fixtures/codex_add.json");

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
        &["codex", "plugin"]
            .iter()
            .chain(rest)
            .copied()
            .collect::<Vec<_>>(),
    )
}

fn install(source: SourceRef, scope: Scope) -> InstallRequest {
    InstallRequest {
        source,
        skills: vec![],
        scope,
        agents: AgentSelection::Detected,
    }
}

#[test]
fn plans() {
    assert_eq!(plan_list().args, strings(&["list", "--json"]));
    assert_eq!(
        plan_available().args,
        strings(&["list", "--available", "--json"])
    );
    assert_eq!(
        plan_add("pdf-kit", "debug").unwrap().args,
        strings(&["add", "pdf-kit@debug", "--json"])
    );
    assert_eq!(
        plan_remove("gmail@openai-curated").unwrap().args,
        strings(&["remove", "gmail@openai-curated", "--json"])
    );
    assert!(plan_add("-x", "debug").is_err());
    assert!(plan_add("a", "b@c").is_err());
    assert!(
        plan_remove("gmail").is_err(),
        "remove needs PLUGIN@MARKETPLACE"
    );
}

#[test]
fn parses_list_and_add_output() {
    let list = parse_codex_list(&rooted(LIST, Path::new("/r"))).unwrap();
    assert_eq!(list.installed.len(), 1);
    assert_eq!(list.installed[0].plugin_id, "gmail@openai-curated");
    assert_eq!(list.installed[0].version.as_deref(), Some("1.4.0"));
    assert_eq!(list.available.len(), 2);
    assert_eq!(
        list.available[0].local_path,
        Some(PathBuf::from("/r/market/plugins/pdf-kit"))
    );
    assert_eq!(list.available[1].local_path, None);
    assert_eq!(parse_add(ADD).as_deref(), Some("pdf-kit@debug"));
    assert_eq!(parse_add("Added plugin"), None);
    assert!(parse_codex_list("[]").is_err());
}

#[test]
fn capabilities_are_limited_to_what_the_cli_offers() {
    let runner = FakeRunner::default();
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    assert_eq!(codex.id(), ChannelId::CodexPlugins);
    let caps = codex.capabilities();
    assert!(caps.search && caps.install && caps.remove);
    assert!(!caps.update && !caps.check);
}

#[test]
fn list_reads_skills_from_the_plugin_cache() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join(".codex");
    skill_dir(
        &home,
        "plugins/cache/openai-curated/gmail/1.4.0/skills/triage",
        "# t",
    );
    let runner = FakeRunner::with(vec![out(0, LIST)]);
    let codex = CodexPlugins::new(&runner, home.clone());
    let skills = codex.list(&Scope::Global).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "triage");
    assert_eq!(
        skills[0].path,
        home.join("plugins/cache/openai-curated/gmail/1.4.0/skills/triage")
    );
    assert_eq!(skills[0].agents, strings(&["codex"]));
    assert_eq!(
        skills[0].source,
        Some(plugin("openai-curated", "gmail", Some("triage")))
    );
    assert_eq!(runner.argvs(), vec![argv(&["list", "--json"])]);
}

#[test]
fn project_scope_has_no_codex_plugins() {
    let runner = FakeRunner::default();
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    assert!(
        codex
            .list(&Scope::Project(PathBuf::from("/p")))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        codex.install(&install(
            plugin("debug", "pdf-kit", None),
            Scope::Project(PathBuf::from("/p"))
        )),
        Err(MarketplaceError::Unsupported(_))
    ));
    assert!(runner.calls().is_empty());
}

#[test]
fn search_filters_available_plugins_and_reads_local_skills() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    skill_dir(r, "market/plugins/pdf-kit/skills/split-pdf", "# s");
    let runner = FakeRunner::with(vec![out(0, &rooted(LIST, r))]);
    let codex = CodexPlugins::new(&runner, r.join(".codex"));
    let results = codex.search("pdf").unwrap();
    let hits: Vec<(&str, &SourceRef)> = results
        .hits
        .iter()
        .map(|h| (h.name.as_str(), &h.source))
        .collect();
    assert_eq!(
        hits,
        vec![
            ("pdf-kit", &plugin("debug", "pdf-kit", None)),
            ("split-pdf", &plugin("debug", "pdf-kit", Some("split-pdf"))),
        ]
    );
    assert_eq!(
        runner.argvs(),
        vec![argv(&["list", "--available", "--json"])]
    );
}

#[test]
fn install_reports_the_plugin_from_add_json() {
    let runner = FakeRunner::with(vec![out(0, ADD)]);
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    let outcome = codex
        .install(&install(plugin("debug", "pdf-kit", None), Scope::Global))
        .unwrap();
    assert_eq!(outcome.changed, strings(&["pdf-kit@debug"]));
    assert_eq!(
        runner.argvs(),
        vec![argv(&["add", "pdf-kit@debug", "--json"])]
    );
    assert!(matches!(
        codex.install(&install(SourceRef::github("o", "r", None), Scope::Global)),
        Err(MarketplaceError::Unsupported(_))
    ));
}

#[test]
fn remove_is_confirmed_by_the_listing() {
    let before = LIST.to_string();
    let after = r#"{"installed":[],"available":[]}"#;
    let removed =
        r#"{"pluginId":"gmail@openai-curated","name":"gmail","marketplaceName":"openai-curated"}"#;
    let runner = FakeRunner::with(vec![out(0, &before), out(0, removed), out(0, after)]);
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    let outcome = codex
        .remove(&RemoveRequest {
            skills: strings(&["gmail@openai-curated"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert_eq!(outcome.changed, strings(&["gmail@openai-curated"]));

    // Codex prints the same JSON when nothing was installed.
    let runner = FakeRunner::with(vec![out(0, after), out(0, removed), out(0, after)]);
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    let outcome = codex
        .remove(&RemoveRequest {
            skills: strings(&["gmail@openai-curated"]),
            scope: Scope::Global,
            agents: AgentSelection::Detected,
        })
        .unwrap();
    assert!(outcome.changed.is_empty());
    assert_eq!(outcome.warnings.len(), 1);
}

#[test]
fn update_hands_off_to_the_plugins_browser() {
    let runner = FakeRunner::default();
    let codex = CodexPlugins::new(&runner, PathBuf::from("/home/dev/.codex"));
    match codex.update(&UpdateRequest {
        skills: vec![],
        scope: Scope::Global,
    }) {
        Err(MarketplaceError::Unsupported(msg)) => assert!(msg.contains("/plugins"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    let infos = codex
        .check(&[agent_marketplace::InstalledSkill {
            name: "triage".to_string(),
            path: PathBuf::from("/x"),
            agents: vec![],
            scope: Scope::Global,
            channel: ChannelId::CodexPlugins,
            source: None,
        }])
        .unwrap();
    assert_eq!(infos[0].available, None);
    assert!(runner.calls().is_empty());
}
