//! One skill space: grouping installs by name and telling identical
//! redundant copies from diverged ones. Temp dirs only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_marketplace::space::{SpaceEntry, SpaceStatus, build_space, redundancy_report};
use agent_marketplace::{ChannelId, InstalledSkill, Scope, SourceRef};

fn install(name: &str, path: &Path, agent: &str, channel: ChannelId) -> InstalledSkill {
    InstalledSkill {
        name: name.to_string(),
        path: path.to_path_buf(),
        agents: vec![agent.to_string()],
        scope: Scope::Global,
        channel,
        source: None,
    }
}

fn skill_dir(root: &Path, rel: &str) -> PathBuf {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("# {rel}\n")).unwrap();
    dir
}

fn no_hash(_: &Path) -> Option<String> {
    None
}

fn entry<'a>(space: &'a [SpaceEntry], name: &str) -> &'a SpaceEntry {
    space
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .unwrap()
}

#[test]
fn single_install_is_single() {
    let tmp = tempfile::tempdir().unwrap();
    let a = skill_dir(tmp.path(), "a");
    let space = build_space(
        &[install("pdf", &a, "claude", ChannelId::NpxSkills)],
        &no_hash,
    );
    assert_eq!(space.len(), 1);
    assert_eq!(space[0].name, "pdf");
    assert_eq!(space[0].status, SpaceStatus::Single);
    assert!(redundancy_report(&space).is_empty());
}

#[test]
fn symlinked_installs_share_a_realpath_and_are_the_same() {
    let tmp = tempfile::tempdir().unwrap();
    let canonical = skill_dir(tmp.path(), "agents/skills/pdf");
    let claude_dir = tmp.path().join("claude/skills");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let link = claude_dir.join("pdf");
    std::os::unix::fs::symlink(&canonical, &link).unwrap();
    let space = build_space(
        &[
            install("pdf", &canonical, "codex", ChannelId::NpxSkills),
            install("PDF", &link, "claude", ChannelId::Local),
        ],
        &no_hash,
    );
    assert_eq!(space.len(), 1);
    assert_eq!(space[0].installs.len(), 2);
    assert_eq!(space[0].status, SpaceStatus::Redundant { same: true });
}

#[test]
fn equal_hashes_are_the_same_and_different_hashes_diverge() {
    let tmp = tempfile::tempdir().unwrap();
    let a = skill_dir(tmp.path(), "one/pdf");
    let b = skill_dir(tmp.path(), "two/pdf");
    let c = skill_dir(tmp.path(), "three/pdf");
    let d = skill_dir(tmp.path(), "one/docx");
    let e = skill_dir(tmp.path(), "two/docx");
    let hashes: HashMap<PathBuf, &str> = [
        (a.clone(), "h1"),
        (b.clone(), "h1"),
        (c.clone(), "h1"),
        (d.clone(), "h2"),
        (e.clone(), "h3"),
    ]
    .into_iter()
    .collect();
    let hash = |p: &Path| hashes.get(p).map(|h| h.to_string());
    let mut pdf_c = install("pdf", &c, "opencode", ChannelId::Skillfish);
    pdf_c.source = Some(SourceRef::github("anthropics", "skills", Some("pdf")));
    let space = build_space(
        &[
            install("pdf", &a, "claude", ChannelId::NpxSkills),
            install("docx", &d, "claude", ChannelId::NpxSkills),
            install("pdf", &b, "codex", ChannelId::ClaudePlugins),
            pdf_c,
            install("docx", &e, "codex", ChannelId::Local),
        ],
        &hash,
    );
    let names: Vec<&str> = space.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["docx", "pdf"]);
    let pdf = entry(&space, "pdf");
    assert_eq!(pdf.installs.len(), 3);
    assert_eq!(pdf.status, SpaceStatus::Redundant { same: true });
    assert_eq!(
        pdf.sources,
        vec![SourceRef::github("anthropics", "skills", Some("pdf"))]
    );
    assert_eq!(
        entry(&space, "docx").status,
        SpaceStatus::Redundant { same: false }
    );
}

#[test]
fn unknown_content_with_different_paths_is_not_claimed_same() {
    let tmp = tempfile::tempdir().unwrap();
    let a = skill_dir(tmp.path(), "a/x");
    let b = skill_dir(tmp.path(), "b/x");
    let space = build_space(
        &[
            install("x", &a, "claude", ChannelId::NpxSkills),
            install("x", &b, "codex", ChannelId::NpxSkills),
        ],
        &no_hash,
    );
    assert_eq!(space[0].status, SpaceStatus::Redundant { same: false });
}

#[test]
fn report_lists_redundant_entries_with_channels_and_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let a = skill_dir(tmp.path(), "a/x");
    let b = skill_dir(tmp.path(), "b/x");
    let c = skill_dir(tmp.path(), "c/y");
    let space = build_space(
        &[
            install("x", &a, "claude", ChannelId::NpxSkills),
            install("x", &b, "codex", ChannelId::Skillfish),
            install("y", &c, "codex", ChannelId::NpxSkills),
        ],
        &|_: &Path| Some("same".to_string()),
    );
    let report = redundancy_report(&space);
    assert_eq!(report.len(), 1);
    let line = &report[0];
    assert_eq!(line.name, "x");
    assert_eq!(line.installs, 2);
    assert!(line.same);
    assert_eq!(
        line.channels,
        vec![ChannelId::NpxSkills, ChannelId::Skillfish]
    );
    assert_eq!(line.agents, vec!["claude".to_string(), "codex".to_string()]);
    assert_eq!(
        line.to_string(),
        "x: 2 installs (identical) via npx-skills, skillfish for claude, codex"
    );
}
