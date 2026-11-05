//! Read-only reader for skillfish install manifests.
//!
//! `skillfish add` writes `.skillfish.json` into each installed skill
//! directory (knoxgraeme/skillfish `src/lib/manifest.ts`: versions 1 and 2
//! are read, 2 is written). Agent skill directories are fixed paths under
//! HOME or the project (`src/lib/agents.ts`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{HomeEnv, skill_dirs};
use crate::types::{ChannelId, InstalledSkill, Scope, SourceRef};

/// The manifest fields this crate uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillfishManifest {
    pub owner: String,
    pub repo: String,
    /// Path inside the repository (`.` for a root skill).
    pub path: Option<String>,
    /// Installed directory name (version 2).
    pub name: Option<String>,
    /// Branch recorded at install time.
    pub branch: Option<String>,
    /// The ref the user pinned (`owner/repo@ref`), if any.
    pub ref_name: Option<String>,
    /// `manual` or `manifest` (installed from a `skillfish.json`; those are
    /// updated by `skillfish install`, never by `skillfish update`).
    pub source: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    version: u64,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    repo: String,
    path: Option<String>,
    name: Option<String>,
    branch: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    source: Option<String>,
}

/// Read `<dir>/.skillfish.json`. `None` when missing, unreadable, not
/// version 1 or 2, or without owner and repo.
pub fn read_skillfish_manifest(dir: &Path) -> Option<SkillfishManifest> {
    let raw = std::fs::read_to_string(dir.join(".skillfish.json")).ok()?;
    let m: Raw = serde_json::from_str(&raw).ok()?;
    if !matches!(m.version, 1 | 2) || m.owner.is_empty() || m.repo.is_empty() {
        return None;
    }
    Some(SkillfishManifest {
        owner: m.owner,
        repo: m.repo,
        path: m.path,
        name: m.name,
        branch: m.branch,
        ref_name: m.ref_name.filter(|r| !r.is_empty()),
        source: m.source,
    })
}

impl SkillfishManifest {
    /// The GitHub source, narrowed to `skill`.
    pub fn source_ref(&self, skill: &str) -> SourceRef {
        SourceRef::github(&self.owner, &self.repo, Some(skill))
    }
}

/// skillfish's directory for each agent id this crate knows: `Claude Code`
/// `.claude/skills`, `Codex` `.codex/skills`, `OpenCode` `.opencode/skills`,
/// under HOME (global) or the project.
fn agent_dirs(base: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("claude", base.join(".claude").join("skills")),
        ("codex", base.join(".codex").join("skills")),
        ("opencode", base.join(".opencode").join("skills")),
    ]
}

/// Skills carrying a skillfish manifest in the known agent directories.
pub fn skillfish_installs(env: &dyn HomeEnv, scope: &Scope) -> Vec<InstalledSkill> {
    let base = match scope {
        Scope::Global => env.home(),
        Scope::Project(root) => root.clone(),
    };
    agent_dirs(&base)
        .into_iter()
        .flat_map(|(agent, dir)| {
            skill_dirs(&dir)
                .into_iter()
                .filter_map(move |(name, path)| {
                    let manifest = read_skillfish_manifest(&path)?;
                    Some(InstalledSkill {
                        source: Some(manifest.source_ref(&name)),
                        name,
                        path,
                        agents: vec![agent.to_string()],
                        scope: scope.clone(),
                        channel: ChannelId::Skillfish,
                    })
                })
        })
        .collect()
}
