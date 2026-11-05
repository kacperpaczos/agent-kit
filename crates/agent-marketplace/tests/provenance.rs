//! Read-only provenance readers (vercel lock, skillfish manifests, Claude
//! installed_plugins.json), the merged view across channels, and the
//! channel list. Every HOME is a temp dir; the real one is never read.

mod common;

use std::path::{Path, PathBuf};

use agent_marketplace::provenance::{
    FixedEnv, HomeEnv, claude_plugin_installs, claude_plugins_root, global_lock_path,
    installed_everywhere, installed_in_project, read_global_lock, read_installed_plugins,
    read_project_lock, read_skillfish_manifest, skillfish_installs, vercel_installs,
};
use agent_marketplace::space::{SpaceStatus, build_space, redundancy_report};
use agent_marketplace::{ChannelId, Scope, SourceKind, SourceRef, all_channels};
use common::{FakeRunner, skill_dir, strings, write};

fn env(home: &Path) -> FixedEnv {
    FixedEnv::new(home)
}

const GLOBAL_LOCK: &str = r#"{
  "version": 3,
  "skills": {
    "pdf": {
      "source": "anthropics/skills",
      "sourceType": "github",
      "sourceUrl": "https://github.com/anthropics/skills.git",
      "skillPath": "skills/pdf/SKILL.md",
      "skillFolderHash": "abc",
      "installedAt": "2026-01-01T00:00:00Z",
      "updatedAt": "2026-01-02T00:00:00Z"
    },
    "gone": {
      "source": "acme/skills",
      "sourceType": "github",
      "sourceUrl": "https://github.com/acme/skills.git",
      "skillFolderHash": "def",
      "installedAt": "2026-01-01T00:00:00Z",
      "updatedAt": "2026-01-01T00:00:00Z"
    }
  },
  "dismissed": {}
}"#;

fn plugins_file(install_path: &Path) -> String {
    format!(
        r#"{{
  "version": 2,
  "plugins": {{
    "doc-tools@acme": [
      {{
        "scope": "user",
        "installPath": "{}",
        "version": "1.2.3",
        "installedAt": "2026-02-03T00:00:00Z",
        "lastUpdated": "2026-02-03T00:00:00Z"
      }}
    ]
  }}
}}"#,
        install_path.display()
    )
}

// ---- env ----

#[test]
fn fixed_env_treats_empty_values_as_unset() {
    let e = FixedEnv::new("/home/dev")
        .with_var("XDG_STATE_HOME", "")
        .with_var("CLAUDE_CONFIG_DIR", "/cfg");
    assert_eq!(e.home(), PathBuf::from("/home/dev"));
    assert_eq!(e.var("XDG_STATE_HOME"), None);
    assert_eq!(e.var("CLAUDE_CONFIG_DIR").as_deref(), Some("/cfg"));
    assert_eq!(e.var("UNSET"), None);
}

// ---- vercel ----

#[test]
fn global_lock_path_follows_xdg_state_home() {
    assert_eq!(
        global_lock_path(&FixedEnv::new("/h")),
        PathBuf::from("/h/.agents/.skill-lock.json")
    );
    assert_eq!(
        global_lock_path(&FixedEnv::new("/h").with_var("XDG_STATE_HOME", "/state")),
        PathBuf::from("/state/skills/.skill-lock.json")
    );
}

#[test]
fn reads_global_and_project_locks_with_warnings_for_bad_files() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    assert!(read_global_lock(&env(h)).entries.is_empty(), "missing file");
    assert_eq!(read_global_lock(&env(h)).warning, None);

    write(&h.join(".agents/.skill-lock.json"), GLOBAL_LOCK);
    let lock = read_global_lock(&env(h));
    assert_eq!(lock.warning, None);
    assert_eq!(lock.entries.len(), 2);
    assert_eq!(lock.entries["pdf"].source, "anthropics/skills");
    assert_eq!(
        lock.entries["pdf"].skill_folder_hash.as_deref(),
        Some("abc")
    );

    write(
        &h.join(".agents/.skill-lock.json"),
        r#"{"version":2,"skills":{}}"#,
    );
    let old = read_global_lock(&env(h));
    assert!(old.entries.is_empty());
    assert!(old.warning.unwrap().contains("version 2"));

    write(&h.join(".agents/.skill-lock.json"), "{oops");
    assert!(read_global_lock(&env(h)).warning.is_some());

    let project = tempfile::tempdir().unwrap();
    write(
        &project.path().join("skills-lock.json"),
        r#"{"version":1,"skills":{"csv":{"source":"o/r","sourceType":"github","computedHash":"h1"}}}"#,
    );
    let lock = read_project_lock(project.path());
    assert_eq!(lock.entries["csv"].skill_folder_hash.as_deref(), Some("h1"));
}

#[test]
fn vercel_installs_are_lock_entries_present_on_disk_with_their_agents() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".agents/.skill-lock.json"), GLOBAL_LOCK);
    let canonical = skill_dir(h, ".agents/skills/pdf", "# pdf");
    std::fs::create_dir_all(h.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink(&canonical, h.join(".claude/skills/pdf")).unwrap();

    let installs = vercel_installs(&env(h), &Scope::Global);
    assert_eq!(installs.len(), 1, "`gone` is not on disk");
    let pdf = &installs[0];
    assert_eq!(pdf.name, "pdf");
    assert_eq!(pdf.path, canonical);
    assert_eq!(pdf.channel, ChannelId::NpxSkills);
    assert_eq!(pdf.agents, strings(&["claude", "codex"]));
    assert_eq!(
        pdf.source,
        Some(SourceRef::github("anthropics", "skills", Some("pdf")))
    );
}

#[test]
fn vercel_agent_dirs_honour_claude_config_dir_and_xdg_config_home() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".agents/.skill-lock.json"), GLOBAL_LOCK);
    skill_dir(h, ".agents/skills/pdf", "# pdf");
    skill_dir(h, "cfg-claude/skills/pdf", "# pdf");
    skill_dir(h, "xdg/opencode/skills/pdf", "# pdf");
    let e = env(h)
        .with_var("CLAUDE_CONFIG_DIR", h.join("cfg-claude").to_str().unwrap())
        .with_var("XDG_CONFIG_HOME", h.join("xdg").to_str().unwrap());
    let installs = vercel_installs(&e, &Scope::Global);
    assert_eq!(
        installs[0].agents,
        strings(&["claude", "codex", "opencode"])
    );
}

#[test]
fn vercel_project_installs_use_the_project_lock() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let p = project.path();
    write(
        &p.join("skills-lock.json"),
        r#"{"version":1,"skills":{"csv":{"source":"/src/csv","sourceType":"local"}}}"#,
    );
    let dir = skill_dir(p, ".agents/skills/csv", "# csv");
    skill_dir(p, ".claude/skills/csv", "# csv copy");
    let installs = vercel_installs(&env(home.path()), &Scope::Project(p.to_path_buf()));
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].path, dir);
    assert_eq!(installs[0].scope, Scope::Project(p.to_path_buf()));
    assert_eq!(
        installs[0].agents,
        strings(&["claude", "codex", "opencode"])
    );
    assert_eq!(
        installs[0].source.as_ref().map(|s| &s.kind),
        Some(&SourceKind::Local {
            path: PathBuf::from("/src/csv")
        })
    );
}

// ---- skillfish ----

#[test]
fn reads_skillfish_manifests_v1_and_v2_only() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let file = d.join(".skillfish.json");
    assert_eq!(read_skillfish_manifest(d), None, "missing");
    write(
        &file,
        r#"{"version":2,"name":"pdf","owner":"ow","repo":"re","path":"skills/pdf","branch":"main","sha":"abc"}"#,
    );
    let m = read_skillfish_manifest(d).unwrap();
    assert_eq!((m.owner.as_str(), m.repo.as_str()), ("ow", "re"));
    assert_eq!(m.path.as_deref(), Some("skills/pdf"));
    assert_eq!(m.name.as_deref(), Some("pdf"));
    assert_eq!(m.source, None);
    write(
        &file,
        r#"{"version":1,"owner":"ow","repo":"re","path":".","branch":"main","sha":"abc","source":"manifest"}"#,
    );
    let v1 = read_skillfish_manifest(d).unwrap();
    assert_eq!(v1.source.as_deref(), Some("manifest"));
    write(&file, r#"{"version":3,"owner":"o","repo":"r"}"#);
    assert_eq!(read_skillfish_manifest(d), None, "unknown version");
    write(&file, r#"{"version":2}"#);
    assert_eq!(read_skillfish_manifest(d), None, "no owner/repo");
    write(&file, "nope");
    assert_eq!(read_skillfish_manifest(d), None, "invalid JSON");
}

#[test]
fn skillfish_installs_scan_agent_dirs_for_manifests() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for rel in [".claude/skills/pdf", ".opencode/skills/pdf"] {
        let dir = skill_dir(h, rel, "# pdf");
        write(
            &dir.join(".skillfish.json"),
            r#"{"version":2,"name":"pdf","owner":"anthropics","repo":"skills","path":"skills/pdf","branch":"main","sha":"a"}"#,
        );
    }
    skill_dir(h, ".claude/skills/untracked", "# u");
    let installs = skillfish_installs(&env(h), &Scope::Global);
    let got: Vec<(&str, &Vec<String>)> = installs
        .iter()
        .map(|i| (i.name.as_str(), &i.agents))
        .collect();
    assert_eq!(
        got,
        vec![
            ("pdf", &strings(&["claude"])),
            ("pdf", &strings(&["opencode"]))
        ]
    );
    assert!(installs.iter().all(|i| i.channel == ChannelId::Skillfish));
    assert_eq!(
        installs[0].source,
        Some(SourceRef::github("anthropics", "skills", Some("pdf")))
    );
}

// ---- Claude installed_plugins.json ----

#[test]
fn claude_plugins_root_follows_env() {
    assert_eq!(
        claude_plugins_root(&FixedEnv::new("/h")),
        PathBuf::from("/h/.claude/plugins")
    );
    assert_eq!(
        claude_plugins_root(&FixedEnv::new("/h").with_var("CLAUDE_CONFIG_DIR", "/c")),
        PathBuf::from("/c/plugins")
    );
    assert_eq!(
        claude_plugins_root(
            &FixedEnv::new("/h")
                .with_var("CLAUDE_CONFIG_DIR", "/c")
                .with_var("CLAUDE_CODE_PLUGIN_CACHE_DIR", "/p")
        ),
        PathBuf::from("/p")
    );
}

#[test]
fn reads_installed_plugins_with_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("installed_plugins.json");
    let (plugins, warning) = read_installed_plugins(&file);
    assert!(plugins.is_empty() && warning.is_none(), "missing");
    write(&file, &plugins_file(Path::new("/cache/doc-tools/1.2.3")));
    let (plugins, warning) = read_installed_plugins(&file);
    assert_eq!(warning, None);
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].plugin, "doc-tools");
    assert_eq!(plugins[0].marketplace, "acme");
    assert_eq!(plugins[0].version, "1.2.3");
    assert_eq!(plugins[0].scope.as_deref(), Some("user"));
    assert_eq!(
        plugins[0].install_path,
        PathBuf::from("/cache/doc-tools/1.2.3")
    );
    write(&file, r#"{"version":1,"plugins":{}}"#);
    assert!(
        read_installed_plugins(&file)
            .1
            .unwrap()
            .contains("version 1")
    );
    write(&file, "{");
    assert!(read_installed_plugins(&file).1.is_some());
}

#[test]
fn claude_plugin_installs_expose_plugin_skills() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let cache = h.join(".claude/plugins/cache/acme/doc-tools/1.2.3");
    skill_dir(&cache, "skills/pdf", "# pdf");
    write(
        &h.join(".claude/plugins/installed_plugins.json"),
        &plugins_file(&cache),
    );
    let (installs, warnings) = claude_plugin_installs(&env(h));
    assert!(warnings.is_empty());
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].name, "pdf");
    assert_eq!(installs[0].path, cache.join("skills/pdf"));
    assert_eq!(installs[0].channel, ChannelId::ClaudePlugins);
    assert_eq!(installs[0].agents, strings(&["claude"]));
    assert_eq!(installs[0].scope, Scope::Global);
}

// ---- merged view ----

#[test]
fn same_skill_via_npx_skills_and_a_claude_plugin_is_redundant() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".agents/.skill-lock.json"), GLOBAL_LOCK);
    skill_dir(h, ".agents/skills/pdf", "# pdf\nsame body\n");
    let cache = h.join(".claude/plugins/cache/acme/doc-tools/1.2.3");
    skill_dir(&cache, "skills/pdf", "# pdf\nsame body\n");
    write(
        &h.join(".claude/plugins/installed_plugins.json"),
        &plugins_file(&cache),
    );

    let installs = installed_everywhere(&env(h));
    let channels: Vec<ChannelId> = installs.iter().map(|i| i.channel).collect();
    assert_eq!(
        channels,
        vec![ChannelId::NpxSkills, ChannelId::ClaudePlugins]
    );

    let by_content = |p: &Path| std::fs::read_to_string(p.join("SKILL.md")).ok();
    let space = build_space(&installs, &by_content);
    assert_eq!(space.len(), 1);
    assert_eq!(space[0].status, SpaceStatus::Redundant { same: true });
    assert_eq!(space[0].sources.len(), 2);
    let report = redundancy_report(&space);
    assert_eq!(
        report[0].channels,
        vec![ChannelId::NpxSkills, ChannelId::ClaudePlugins]
    );
}

#[test]
fn installed_in_project_reads_project_channels_only() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let p = project.path();
    write(
        &p.join("skills-lock.json"),
        r#"{"version":1,"skills":{"csv":{"source":"o/r","sourceType":"github"}}}"#,
    );
    skill_dir(p, ".agents/skills/csv", "# csv");
    let fish = skill_dir(p, ".claude/skills/pdf", "# pdf");
    write(
        &fish.join(".skillfish.json"),
        r#"{"version":2,"name":"pdf","owner":"o","repo":"r","path":".","branch":"main","sha":"a"}"#,
    );
    let installs = installed_in_project(&env(home.path()), p);
    let names: Vec<(&str, ChannelId)> = installs
        .iter()
        .map(|i| (i.name.as_str(), i.channel))
        .collect();
    assert_eq!(
        names,
        vec![("csv", ChannelId::NpxSkills), ("pdf", ChannelId::Skillfish)]
    );
    assert!(
        installs
            .iter()
            .all(|i| i.scope == Scope::Project(p.to_path_buf()))
    );
}

#[test]
fn all_channels_lists_every_adapter_in_order() {
    let runner = FakeRunner::default();
    let home = tempfile::tempdir().unwrap();
    let channels = all_channels(&runner, &env(home.path()));
    let ids: Vec<ChannelId> = channels.iter().map(|c| c.id()).collect();
    assert_eq!(
        ids,
        vec![
            ChannelId::NpxSkills,
            ChannelId::Skillfish,
            ChannelId::ClaudePlugins,
            ChannelId::CodexPlugins,
        ]
    );
    assert!(runner.calls().is_empty(), "building runs nothing");
}

#[test]
fn plugins_sharing_one_install_dir_keep_both_identities() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("installed_plugins.json");
    let record = r#"[{"scope":"user","installPath":"/cache/shared","version":"1.0.0"}]"#;
    write(
        &file,
        &format!(
            r#"{{"version":2,"plugins":{{"a@m1":{record},"b@m2":{record},"a@m1x":{record}}}}}"#
        ),
    );
    let (plugins, warning) = read_installed_plugins(&file);
    assert_eq!(warning, None);
    let ids: Vec<String> = plugins
        .iter()
        .map(|p| format!("{}@{}", p.plugin, p.marketplace))
        .collect();
    assert_eq!(ids, strings(&["a@m1", "a@m1x", "b@m2"]));
}
