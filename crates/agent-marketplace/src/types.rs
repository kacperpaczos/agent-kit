//! The channel-neutral model: where a skill comes from, where it is
//! installed, what a marketplace can do, and the [`Marketplace`] trait.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The distribution channel an installation came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelId {
    /// vercel-labs/skills (`npx skills`).
    NpxSkills,
    /// skillfish (`npx skillfish`).
    Skillfish,
    /// Claude Code plugin marketplaces.
    ClaudePlugins,
    /// Codex plugins.
    CodexPlugins,
    /// A copy on disk that no marketplace tracks.
    Local,
}

impl ChannelId {
    /// Stable machine name, e.g. `npx-skills`.
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelId::NpxSkills => "npx-skills",
            ChannelId::Skillfish => "skillfish",
            ChannelId::ClaudePlugins => "claude-plugins",
            ChannelId::CodexPlugins => "codex-plugins",
            ChannelId::Local => "local",
        }
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a skill's content comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SourceKind {
    /// A GitHub repository, `owner/repo`.
    Github { owner: String, repo: String },
    /// Any other URL (git, GitLab, well-known endpoint, download).
    Url { url: String },
    /// A directory on disk.
    Local { path: PathBuf },
    /// A plugin from a plugin marketplace.
    Plugin { marketplace: String, plugin: String },
}

/// A source plus, optionally, one skill inside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub kind: SourceKind,
    pub skill: Option<String>,
}

impl SourceRef {
    /// `owner/repo` on GitHub, optionally narrowed to one skill.
    pub fn github(owner: &str, repo: &str, skill: Option<&str>) -> SourceRef {
        SourceRef {
            kind: SourceKind::Github {
                owner: owner.to_string(),
                repo: repo.to_string(),
            },
            skill: skill.map(str::to_string),
        }
    }
}

/// Installation scope: the user's global directories or one project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Global,
    Project(PathBuf),
}

/// One search result from a marketplace catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub name: String,
    pub source: SourceRef,
    /// `None` when the channel does not report descriptions.
    pub description: Option<String>,
    /// Install count; approximate when the channel rounds it (`1.2K`).
    pub installs: Option<u64>,
    pub url: Option<String>,
}

/// Search results plus non-fatal problems (e.g. unrecognised output).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub warnings: Vec<String>,
}

/// One installed skill as a channel reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub name: String,
    pub path: PathBuf,
    /// Agent ids (`claude`, `codex`, `opencode`) where known, otherwise the
    /// channel's own agent name.
    pub agents: Vec<String>,
    pub scope: Scope,
    pub channel: ChannelId,
    pub source: Option<SourceRef>,
}

/// What a marketplace adapter supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub search: bool,
    pub install: bool,
    pub remove: bool,
    pub update: bool,
    pub check: bool,
}

/// Update availability for one skill; `None` when it could not be checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub name: String,
    pub available: Option<bool>,
}

/// Which agents a mutating operation targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentSelection {
    /// Let the channel decide (its detected or default agents).
    Detected,
    /// Every agent the channel supports.
    All,
    /// These agent ids (`claude`, `codex`, `opencode`) or channel agent names.
    Only(Vec<String>),
}

/// Install skills from a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    pub source: SourceRef,
    /// Skill names to install; empty means `source.skill`, or the channel's
    /// default when that is also absent. `*` selects every skill.
    pub skills: Vec<String>,
    pub scope: Scope,
    pub agents: AgentSelection,
}

/// Remove installed skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveRequest {
    /// At least one name; `*` removes every skill.
    pub skills: Vec<String>,
    pub scope: Scope,
    pub agents: AgentSelection,
}

/// Update installed skills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// Empty means every skill in the scope.
    pub skills: Vec<String>,
    pub scope: Scope,
}

/// Result of a mutating operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// Names of the skills that changed.
    pub changed: Vec<String>,
    pub warnings: Vec<String>,
}

/// Why a marketplace operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketplaceError {
    /// A user value was rejected before anything ran.
    Invalid(String),
    /// The channel cannot do this (e.g. a plugin source for `npx skills`).
    Unsupported(String),
    /// The command could not be run (not found, timeout, I/O).
    Run(String),
    /// The caller cancelled the command.
    Cancelled,
    /// The command succeeded but its output was not understood.
    Parse(String),
    /// The command ran and failed.
    Failed { status: Option<i32>, stderr: String },
}

impl fmt::Display for MarketplaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarketplaceError::Invalid(msg) => write!(f, "invalid input: {msg}"),
            MarketplaceError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            MarketplaceError::Run(msg) => write!(f, "could not run command: {msg}"),
            MarketplaceError::Parse(msg) => write!(f, "{msg}"),
            MarketplaceError::Cancelled => f.write_str("cancelled"),
            MarketplaceError::Failed { status, stderr } => match status {
                Some(code) => write!(f, "command failed with exit code {code}: {stderr}"),
                None => write!(f, "command was killed: {stderr}"),
            },
        }
    }
}

impl std::error::Error for MarketplaceError {}

pub type Result<T> = std::result::Result<T, MarketplaceError>;

/// A skill marketplace channel. Blocking: every call runs its command
/// through the adapter's injected runner and returns when it exits.
pub trait Marketplace {
    fn id(&self) -> ChannelId;
    fn capabilities(&self) -> Capabilities;
    fn search(&self, query: &str) -> Result<SearchResults>;
    fn list(&self, scope: &Scope) -> Result<Vec<InstalledSkill>>;
    /// One entry per given skill, in input order.
    fn check(&self, installed: &[InstalledSkill]) -> Result<Vec<UpdateInfo>>;
    fn install(&self, req: &InstallRequest) -> Result<Outcome>;
    fn remove(&self, req: &RemoveRequest) -> Result<Outcome>;
    fn update(&self, req: &UpdateRequest) -> Result<Outcome>;
}
