//! Interactive terminal launch recipes from `[agent.terminal]` tables.
//!
//! A host app that embeds a terminal can open any agent interactively in a
//! working directory, with a context text and an approval mode, without
//! hard-coding agent flags. Everything is an argv vector: no shell is
//! involved, so the context text is passed verbatim as one argument.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::registry::AgentDef;

/// How much the agent may do without asking the user.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Edits are applied without asking.
    Auto,
    /// The agent asks before acting.
    Ask,
    /// Read-only planning, no edits.
    Plan,
}

/// How the context text reaches the agent's argv.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextDef {
    /// `[flag, context]`, appended to the system prompt.
    SystemPromptFlag { flag: String },
    /// `[flag, context]`, used as the first user prompt.
    PromptFlag { flag: String },
    /// `[context]` as the last positional argument.
    FirstArg,
    /// The context is not passed in argv; the host types it in.
    #[default]
    None,
}

/// An `[agent.terminal]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TerminalDef {
    /// Fixed leading args (usually empty).
    #[serde(default)]
    pub args: Vec<String>,
    /// Flag naming the working directory (e.g. `-C`); `None` relies on the
    /// process cwd alone.
    #[serde(default)]
    pub cwd_flag: Option<String>,
    /// How the context text is passed.
    #[serde(default)]
    pub context: ContextDef,
    /// Extra args per supported mode; a missing mode is unsupported.
    #[serde(default)]
    pub modes: BTreeMap<ApprovalMode, Vec<String>>,
}

/// A ready-to-spawn interactive launch: run `program` with `args` in `cwd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The applied mode; `None` when the requested mode is unsupported and
    /// the agent starts with its own defaults.
    pub mode: Option<ApprovalMode>,
}

/// Build the interactive launch of `def` from its resolved binary `bin`.
///
/// argv = recipe args + `cwd_flag cwd` (if any) + mode args (if the mode
/// is supported) + context per [`ContextDef`]. An agent without a terminal
/// table starts as plain `bin` with no args and no mode; the context is
/// then not in argv (see [`context_in_argv`]).
pub fn terminal_launch(
    def: &AgentDef,
    bin: &str,
    cwd: &Path,
    context: &str,
    mode: ApprovalMode,
) -> TerminalLaunch {
    let mut launch = TerminalLaunch {
        program: bin.to_string(),
        args: Vec::new(),
        cwd: cwd.to_path_buf(),
        mode: None,
    };
    let Some(terminal) = &def.terminal else {
        return launch;
    };
    launch.args.extend(terminal.args.iter().cloned());
    if let Some(flag) = &terminal.cwd_flag {
        launch.args.push(flag.clone());
        launch.args.push(cwd.to_string_lossy().into_owned());
    }
    if let Some(mode_args) = terminal.modes.get(&mode) {
        launch.args.extend(mode_args.iter().cloned());
        launch.mode = Some(mode);
    }
    match &terminal.context {
        ContextDef::SystemPromptFlag { flag } | ContextDef::PromptFlag { flag } => {
            launch.args.push(flag.clone());
            launch.args.push(context.to_string());
        }
        ContextDef::FirstArg => launch.args.push(context.to_string()),
        ContextDef::None => {}
    }
    launch
}

/// Whether [`terminal_launch`] puts the context in argv. When `false`, the
/// host should send the context as the first message once the agent starts.
pub fn context_in_argv(def: &AgentDef) -> bool {
    def.terminal
        .as_ref()
        .is_some_and(|t| t.context != ContextDef::None)
}

/// Modes the agent's recipe supports, in `Auto`, `Ask`, `Plan` order.
pub fn supported_modes(def: &AgentDef) -> Vec<ApprovalMode> {
    def.terminal
        .as_ref()
        .map(|t| t.modes.keys().copied().collect())
        .unwrap_or_default()
}
