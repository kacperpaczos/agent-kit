//! Injectable environment: the only place that reads `HOME` and agent
//! location variables, so tests never see the real home directory.

#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::path::PathBuf;

/// Source of the home directory and environment variables.
pub trait Env {
    /// Home directory; `None` when unknown.
    fn home(&self) -> Option<PathBuf>;
    /// Value of an environment variable; an empty value counts as unset.
    fn var(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeEnv;

impl Env for NativeEnv {
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }

    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

/// A fixed environment for tests and embedding hosts.
#[derive(Debug, Default, Clone)]
pub struct FakeEnv {
    pub home: Option<PathBuf>,
    pub vars: HashMap<String, String>,
}

impl FakeEnv {
    /// An environment whose home is `home` and with no variables set.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            vars: HashMap::new(),
        }
    }

    /// Builder: set one variable.
    pub fn var(mut self, key: &str, value: impl Into<String>) -> Self {
        self.vars.insert(key.to_string(), value.into());
        self
    }
}

impl Env for FakeEnv {
    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn var(&self, key: &str) -> Option<String> {
        self.vars.get(key).filter(|v| !v.is_empty()).cloned()
    }
}

/// Where each agent keeps its transcripts. Mirrors the `transcripts`
/// entries of agent-detect's `agents.toml`; a host may also fill it from
/// its own registry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Roots {
    /// Claude Code `projects` directory (`<slug>/<uuid>.jsonl`).
    pub claude_projects: Option<PathBuf>,
    /// Codex `sessions` directory (`YYYY/MM/DD/rollout-*.jsonl`).
    pub codex_sessions: Option<PathBuf>,
    /// OpenCode SQLite database.
    pub opencode_db: Option<PathBuf>,
}

impl Roots {
    /// Default locations:
    /// `$CLAUDE_CONFIG_DIR/projects || ~/.claude/projects`,
    /// `$CODEX_HOME/sessions || ~/.codex/sessions`,
    /// `$XDG_DATA_HOME/opencode/opencode.db || ~/.local/share/opencode/opencode.db`.
    pub fn from_env(env: &dyn Env) -> Self {
        let home = env.home();
        let pick = |var: &str, suffix: &str, fallback: &str| -> Option<PathBuf> {
            match env.var(var) {
                Some(base) => Some(PathBuf::from(base).join(suffix)),
                None => home.as_ref().map(|h| h.join(fallback)),
            }
        };
        Self {
            claude_projects: pick("CLAUDE_CONFIG_DIR", "projects", ".claude/projects"),
            codex_sessions: pick("CODEX_HOME", "sessions", ".codex/sessions"),
            opencode_db: pick(
                "XDG_DATA_HOME",
                "opencode/opencode.db",
                ".local/share/opencode/opencode.db",
            ),
        }
    }
}
