//! Read-only readers for vercel-labs/skills (`npx skills`) lock files.
//!
//! Global lock: `$XDG_STATE_HOME/skills/.skill-lock.json`, else
//! `~/.agents/.skill-lock.json` (version 3; upstream discards older ones).
//! Project lock: `<root>/skills-lock.json` (version 1). Canonical copies
//! live in `~/.agents/skills/<name>` or `<root>/.agents/skills/<name>`
//! (vercel-labs/skills `src/skill-lock.ts`, `src/installer.ts`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{HomeEnv, dir_var};
use crate::npx_skills::parse::source_from_lock;
use crate::types::{ChannelId, InstalledSkill, Scope};

/// Oldest global lock version upstream keeps.
const GLOBAL_VERSION: u64 = 3;
/// The only project lock version upstream accepts.
const PROJECT_VERSION: u64 = 1;

/// One skill entry of a lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VercelEntry {
    pub source: String,
    pub source_type: String,
    pub source_url: Option<String>,
    pub skill_path: Option<String>,
    /// `skillFolderHash` (global) or `computedHash` (project).
    pub skill_folder_hash: Option<String>,
    pub updated_at: Option<String>,
}

/// A parsed lock: entries by skill name, plus a warning when the file
/// exists but could not be used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VercelLock {
    pub entries: BTreeMap<String, VercelEntry>,
    pub warning: Option<String>,
}

#[derive(Deserialize)]
struct LockFile {
    version: u64,
    #[serde(default)]
    skills: BTreeMap<String, LockEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockEntry {
    #[serde(default)]
    source: String,
    #[serde(default)]
    source_type: String,
    source_url: Option<String>,
    skill_path: Option<String>,
    #[serde(alias = "computedHash")]
    skill_folder_hash: Option<String>,
    updated_at: Option<String>,
}

/// Where the global lock lives for `env`.
pub fn global_lock_path(env: &dyn HomeEnv) -> PathBuf {
    match env.var("XDG_STATE_HOME") {
        Some(dir) => PathBuf::from(dir).join("skills").join(".skill-lock.json"),
        None => env.home().join(".agents").join(".skill-lock.json"),
    }
}

/// Read the global lock. Missing file: empty, no warning. Unusable file
/// (unreadable, invalid JSON, version below 3): empty with a warning.
pub fn read_global_lock(env: &dyn HomeEnv) -> VercelLock {
    read_lock(
        &global_lock_path(env),
        |v| v >= GLOBAL_VERSION,
        GLOBAL_VERSION,
    )
}

/// Read `<root>/skills-lock.json` (version 1); same warning rules.
pub fn read_project_lock(root: &Path) -> VercelLock {
    read_lock(
        &root.join("skills-lock.json"),
        |v| v == PROJECT_VERSION,
        PROJECT_VERSION,
    )
}

fn read_lock(path: &Path, accept: impl Fn(u64) -> bool, expected: u64) -> VercelLock {
    let warn = |msg: String| VercelLock {
        entries: BTreeMap::new(),
        warning: Some(format!("{msg} ({})", path.display())),
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return VercelLock::default(),
        Err(e) => return warn(format!("lock file unreadable: {e}")),
    };
    match serde_json::from_str::<LockFile>(&raw) {
        Ok(file) if accept(file.version) => VercelLock {
            entries: file
                .skills
                .into_iter()
                .map(|(name, e)| {
                    let entry = VercelEntry {
                        source: e.source,
                        source_type: e.source_type,
                        source_url: e.source_url,
                        skill_path: e.skill_path,
                        skill_folder_hash: e.skill_folder_hash,
                        updated_at: e.updated_at,
                    };
                    (name, entry)
                })
                .collect(),
            warning: None,
        },
        Ok(file) => warn(format!(
            "unsupported lock version {} (expected {expected})",
            file.version
        )),
        Err(e) => warn(format!("invalid lock file: {e}")),
    }
}

/// Agent directories `npx skills` links a skill into, per agent id
/// (vercel-labs/skills `src/agents.ts`): global `claude-code` uses
/// `$CLAUDE_CONFIG_DIR/skills` or `~/.claude/skills`, `codex` the canonical
/// `~/.agents/skills`, `opencode` `$XDG_CONFIG_HOME/opencode/skills` or
/// `~/.config/opencode/skills`; in a project `.claude/skills`, and
/// `.agents/skills` for both others.
fn agent_dirs(env: &dyn HomeEnv, scope: &Scope) -> [(&'static str, PathBuf); 3] {
    match scope {
        Scope::Global => {
            let home = env.home();
            let claude = dir_var(env, "CLAUDE_CONFIG_DIR", home.join(".claude"));
            let config = dir_var(env, "XDG_CONFIG_HOME", home.join(".config"));
            [
                ("claude", claude.join("skills")),
                ("codex", home.join(".agents").join("skills")),
                ("opencode", config.join("opencode").join("skills")),
            ]
        }
        Scope::Project(root) => [
            ("claude", root.join(".claude").join("skills")),
            ("codex", root.join(".agents").join("skills")),
            ("opencode", root.join(".agents").join("skills")),
        ],
    }
}

/// Lock entries whose canonical copy exists on disk, as installations of
/// the `npx skills` channel. `agents` lists every agent whose skills
/// directory holds the skill (a link or a copy).
pub fn vercel_installs(env: &dyn HomeEnv, scope: &Scope) -> Vec<InstalledSkill> {
    let (lock, base) = match scope {
        Scope::Global => (read_global_lock(env), env.home()),
        Scope::Project(root) => (read_project_lock(root), root.clone()),
    };
    let canonical = base.join(".agents").join("skills");
    let agents = agent_dirs(env, scope);
    lock.entries
        .into_iter()
        .filter_map(|(name, entry)| {
            let path = canonical.join(&name);
            if !path.join("SKILL.md").is_file() {
                return None;
            }
            let source = source_from_lock(
                &name,
                Some(&entry.source),
                entry.source_url.as_deref(),
                Some(&entry.source_type),
            );
            Some(InstalledSkill {
                agents: agents
                    .iter()
                    .filter(|(_, dir)| dir.join(&name).join("SKILL.md").is_file())
                    .map(|(id, _)| id.to_string())
                    .collect(),
                name,
                path,
                scope: scope.clone(),
                channel: ChannelId::NpxSkills,
                source,
            })
        })
        .collect()
}
