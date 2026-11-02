//! Agent lifecycle state and detection outcome.

use std::path::PathBuf;

use serde::Serialize;

/// Agent lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum AgentState {
    Absent,
    ConfigOnly { config_dir: PathBuf },
    BinaryOnly { bin: PathBuf },
    Runnable { bin: PathBuf, version: String },
    Ready { bin: PathBuf, version: String },
}

/// Detection outcome for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct AgentStatus {
    pub agent_id: String,
    pub state: AgentState,
    pub diagnostics: Vec<String>,
    pub searched_dirs: Vec<PathBuf>,
}
