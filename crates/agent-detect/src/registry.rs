//! Agent definitions deserialized from `[[agent]]` tables in `agents.toml`.

use std::sync::LazyLock;

use crate::terminal::TerminalDef;
use crate::trust::TrustDef;

/// The bundled agent list, parsed once from `agents.toml` next to this
/// crate's `Cargo.toml`. A parse failure is a bug caught by tests, so
/// failing here panics.
pub fn builtin_agents() -> &'static [AgentDef] {
    static AGENTS: LazyLock<Vec<AgentDef>> =
        LazyLock::new(|| match parse_agents(include_str!("../agents.toml")) {
            Ok(agents) => agents,
            Err(err) => panic!("invalid builtin agents.toml: {err}"),
        });
    &AGENTS
}

/// Parse a `[[agent]]` document so other apps can ship their own list.
///
/// Returns `Err(message)` for malformed TOML or missing required fields.
pub fn parse_agents(toml: &str) -> Result<Vec<AgentDef>, String> {
    #[derive(serde::Deserialize)]
    struct AgentsFile {
        agent: Vec<AgentDef>,
    }
    toml::from_str::<AgentsFile>(toml)
        .map(|file| file.agent)
        .map_err(|err| err.to_string())
}

/// An engine agent definition.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct AgentDef {
    pub id: String,
    pub name: String,
    pub bin: String,
    #[serde(default)]
    pub fallback_bins: Vec<String>,
    pub bin_env: Option<String>,
    pub config_dirs: Vec<String>,
    pub version_args: Vec<String>,
    pub version_regex: Option<String>,
    pub auth_probe: Option<Vec<String>>,
    pub transport: String,
    pub models: Option<ModelsDef>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    pub sessions: Option<SessionsDef>,
    /// Where the agent keeps conversation transcripts; separate from
    /// `sessions`, which feeds project detection.
    #[serde(default)]
    pub transcripts: Option<TranscriptsDef>,
    /// Interactive terminal launch recipe; `None` = plain binary fallback.
    #[serde(default)]
    pub terminal: Option<TerminalDef>,
    /// Per-folder trust store; `None` = the agent has no trust concept.
    #[serde(default)]
    pub trust: Option<TrustDef>,
}

/// Command listing an agent's models, plus its output parser name.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ModelsDef {
    pub args: Vec<String>,
    pub parser: String,
}

/// Saved-session store of an agent, plus its parser name
/// (`claude-json` | `codex-jsonl` | `opencode-sqlite`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct SessionsDef {
    pub path: String,
    pub parser: String,
}

/// Transcript store of an agent: a location template (see
/// [`crate::paths::expand`]) and its format
/// (`claude-jsonl` | `codex-rollout` | `opencode-sqlite`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct TranscriptsDef {
    pub path: String,
    pub format: String,
}
