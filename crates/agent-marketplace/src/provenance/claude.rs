//! Read-only reader for Claude Code's `installed_plugins.json` (version 2).
//!
//! The plugins root is `$CLAUDE_CODE_PLUGIN_CACHE_DIR`, else
//! `$CLAUDE_CONFIG_DIR/plugins`, else `~/.claude/plugins`
//! (code.claude.com/docs/en/plugins/loading, /env-vars). Each record names
//! the plugin's `installPath`; its skills are `skills/<name>/SKILL.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{HomeEnv, dir_var, plugin_skill_dirs};
use crate::types::{ChannelId, InstalledSkill, Scope, SourceKind, SourceRef};

const PLUGINS_VERSION: u64 = 2;

/// One install record of a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub plugin: String,
    pub marketplace: String,
    pub version: String,
    pub install_path: PathBuf,
    /// `user`, `project`, `local` or `managed` when recorded.
    pub scope: Option<String>,
    pub project_path: Option<PathBuf>,
    pub last_updated: Option<String>,
}

impl InstalledPlugin {
    /// [`Scope::Project`] for project/local records with a project path,
    /// otherwise global.
    pub fn scope(&self) -> Scope {
        match (self.scope.as_deref(), &self.project_path) {
            (Some("project" | "local"), Some(dir)) => Scope::Project(dir.clone()),
            _ => Scope::Global,
        }
    }
}

#[derive(Deserialize)]
struct PluginsFile {
    version: u64,
    #[serde(default)]
    plugins: BTreeMap<String, Vec<Record>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    install_path: PathBuf,
    #[serde(default)]
    version: String,
    scope: Option<String>,
    project_path: Option<PathBuf>,
    last_updated: Option<String>,
}

/// The Claude Code plugins root for `env`.
pub fn claude_plugins_root(env: &dyn HomeEnv) -> PathBuf {
    let config = dir_var(env, "CLAUDE_CONFIG_DIR", env.home().join(".claude"));
    dir_var(env, "CLAUDE_CODE_PLUGIN_CACHE_DIR", config.join("plugins"))
}

/// Read every install record in `file`. Missing file: empty, no warning.
/// Unusable file: empty with a warning naming it.
pub fn read_installed_plugins(file: &Path) -> (Vec<InstalledPlugin>, Option<String>) {
    let warn = |msg: String| (Vec::new(), Some(format!("{msg} ({})", file.display())));
    let raw = match std::fs::read_to_string(file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(e) => return warn(format!("file unreadable: {e}")),
    };
    match serde_json::from_str::<PluginsFile>(&raw) {
        Ok(parsed) if parsed.version == PLUGINS_VERSION => {
            let mut plugins = Vec::new();
            for (key, records) in parsed.plugins {
                let Some((plugin, marketplace)) = key.split_once('@') else {
                    continue;
                };
                for r in records {
                    let duplicate = plugins.iter().any(|p: &InstalledPlugin| {
                        p.install_path == r.install_path
                            && p.scope == r.scope
                            && p.project_path == r.project_path
                    });
                    if duplicate || r.install_path.as_os_str().is_empty() {
                        continue;
                    }
                    plugins.push(InstalledPlugin {
                        plugin: plugin.to_string(),
                        marketplace: marketplace.to_string(),
                        version: r.version,
                        install_path: r.install_path,
                        scope: r.scope,
                        project_path: r.project_path,
                        last_updated: r.last_updated,
                    });
                }
            }
            (plugins, None)
        }
        Ok(parsed) => warn(format!(
            "unsupported version {} (expected {PLUGINS_VERSION})",
            parsed.version
        )),
        Err(e) => warn(format!("invalid file: {e}")),
    }
}

/// Skills inside every recorded plugin, as Claude plugin installations,
/// plus the reader's warnings.
pub fn claude_plugin_installs(env: &dyn HomeEnv) -> (Vec<InstalledSkill>, Vec<String>) {
    let file = claude_plugins_root(env).join("installed_plugins.json");
    let (plugins, warning) = read_installed_plugins(&file);
    let installs = plugins
        .iter()
        .flat_map(|p| {
            plugin_skill_dirs(&p.install_path, &p.plugin)
                .into_iter()
                .map(move |(name, path)| InstalledSkill {
                    source: Some(SourceRef {
                        kind: SourceKind::Plugin {
                            marketplace: p.marketplace.clone(),
                            plugin: p.plugin.clone(),
                        },
                        skill: Some(name.clone()),
                    }),
                    name,
                    path,
                    agents: vec!["claude".to_string()],
                    scope: p.scope(),
                    channel: ChannelId::ClaudePlugins,
                })
        })
        .collect();
    (installs, warning.into_iter().collect())
}
