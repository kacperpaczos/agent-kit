//! Read-only provenance: what each channel's own records say is installed,
//! without running any CLI.
//!
//! - [`vercel`]: the `npx skills` lock files.
//! - [`skillfish`]: `.skillfish.json` manifests inside skill directories.
//! - [`claude`]: Claude Code's `installed_plugins.json`.
//!
//! These files belong to their tools; nothing here writes them. Every
//! reader takes an injectable [`HomeEnv`], so tests never see the real HOME.

pub mod claude;
pub mod skillfish;
pub mod vercel;

use std::path::{Path, PathBuf};

pub use claude::{
    InstalledPlugin, claude_plugin_installs, claude_plugins_root, read_installed_plugins,
};
pub use skillfish::{SkillfishManifest, read_skillfish_manifest, skillfish_installs};
pub use vercel::{
    VercelEntry, VercelLock, global_lock_path, read_global_lock, read_project_lock, vercel_installs,
};

use crate::types::{InstalledSkill, Scope};

/// Home directory and environment variables, injectable for tests.
pub trait HomeEnv {
    fn home(&self) -> PathBuf;
    /// Value of an environment variable; an empty value counts as unset.
    fn var(&self, key: &str) -> Option<String>;
}

/// The process environment (`HOME` and real variables).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEnv;

// The one place in this crate that reads the process environment.
#[allow(clippy::disallowed_methods)]
impl HomeEnv for SystemEnv {
    fn home(&self) -> PathBuf {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

/// A fixed HOME and variable set (tests, sandboxes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedEnv {
    home: PathBuf,
    vars: Vec<(String, String)>,
}

impl FixedEnv {
    pub fn new(home: impl AsRef<Path>) -> FixedEnv {
        FixedEnv {
            home: home.as_ref().to_path_buf(),
            vars: Vec::new(),
        }
    }

    /// Set (or replace) one variable.
    pub fn with_var(mut self, key: &str, value: &str) -> FixedEnv {
        self.vars.retain(|(k, _)| k != key);
        self.vars.push((key.to_string(), value.to_string()));
        self
    }
}

impl HomeEnv for FixedEnv {
    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn var(&self, key: &str) -> Option<String> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    }
}

/// `$VAR` as a path when set, else `fallback`.
pub(crate) fn dir_var(env: &dyn HomeEnv, key: &str, fallback: PathBuf) -> PathBuf {
    env.var(key).map(PathBuf::from).unwrap_or(fallback)
}

/// Codex home: `$CODEX_HOME` or `~/.codex`.
pub fn codex_home(env: &dyn HomeEnv) -> PathBuf {
    dir_var(env, "CODEX_HOME", env.home().join(".codex"))
}

/// Skill directories directly under `dir`: name not starting with `.`,
/// a directory (symlinks followed) holding `SKILL.md`. Sorted by name.
pub fn skill_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let path = entry.path();
            (!name.starts_with('.') && path.join("SKILL.md").is_file()).then_some((name, path))
        })
        .collect();
    found.sort();
    found
}

/// Skills a plugin contributes from its default layout: each
/// `skills/<name>/SKILL.md`, or — with no `skills/` skills — a `SKILL.md`
/// at the plugin root as one skill named after the plugin. Skill paths a
/// manifest adds elsewhere are not followed.
pub fn plugin_skill_dirs(root: &Path, plugin: &str) -> Vec<(String, PathBuf)> {
    let skills = skill_dirs(&root.join("skills"));
    if skills.is_empty() && root.join("SKILL.md").is_file() {
        return vec![(plugin.to_string(), root.to_path_buf())];
    }
    skills
}

/// Every global installation the channels' own records show: `npx skills`
/// lock, skillfish manifests, Claude plugins. No command is run; unusable
/// records are skipped (their readers report why).
pub fn installed_everywhere(env: &dyn HomeEnv) -> Vec<InstalledSkill> {
    let mut all = vercel_installs(env, &Scope::Global);
    all.extend(skillfish_installs(env, &Scope::Global));
    all.extend(
        claude_plugin_installs(env)
            .0
            .into_iter()
            .filter(|i| i.scope == Scope::Global),
    );
    all
}

/// Installations recorded for one project: its `skills-lock.json`,
/// skillfish manifests in its agent directories, and Claude plugins
/// installed at project or local scope for it.
pub fn installed_in_project(env: &dyn HomeEnv, root: &Path) -> Vec<InstalledSkill> {
    let scope = Scope::Project(root.to_path_buf());
    let mut all = vercel_installs(env, &scope);
    all.extend(skillfish_installs(env, &scope));
    all.extend(
        claude_plugin_installs(env)
            .0
            .into_iter()
            .filter(|i| i.scope == scope),
    );
    all
}
