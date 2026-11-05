//! Read-only transcript readers for AI-agent CLIs.
//!
//! - **Claude Code**: `<projects>/<cwd-slug>/<uuid>.jsonl`, subagents in
//!   `<uuid>/subagents/*.jsonl`.
//! - **Codex**: `<sessions>/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
//! - **OpenCode**: the `session_v2` / `session_message` (and, when present,
//!   `session_part`) tables of `opencode.db`, opened read-only (cargo
//!   feature `opencode`, on by default).
//!
//! Nothing here writes anywhere. Listing reads only the first and last
//! lines of each file (or one metadata query); reading events streams the
//! file line by line, skips broken lines with a warning, and caps every
//! text and the whole result.

mod claude;
mod codex;
pub mod env;
mod infer;
mod lines;
#[cfg(feature = "opencode")]
mod opencode;
pub mod redact;
pub mod time;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use env::{Env, FakeEnv, NativeEnv, Roots};
pub use redact::redact;

/// The agents whose transcripts can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentKind {
    Claude,
    Codex,
    OpenCode,
}

impl AgentKind {
    pub const ALL: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Codex, AgentKind::OpenCode];

    /// The agent id used by agent-detect (`claude`, `codex`, `opencode`).
    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// One saved conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub agent: AgentKind,
    /// Claude: file stem (session uuid, or `agent-…` for a subagent);
    /// Codex: thread id; OpenCode: session id.
    pub id: String,
    pub cwd: Option<PathBuf>,
    pub started: Option<SystemTime>,
    pub ended: Option<SystemTime>,
    /// Transcript file (Claude, Codex) or database (OpenCode).
    pub path: PathBuf,
    /// A subagent / child conversation rather than a top-level one.
    pub sidechain: bool,
    /// The parent conversation of a sidechain, when recorded.
    pub parent_id: Option<String>,
    /// Title, when the agent stores one (OpenCode).
    pub title: Option<String>,
}

/// Who produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    User,
    Assistant,
    /// Developer / system / synthetic messages injected by the agent.
    System,
    /// Tool output fed back to the model.
    Tool,
}

/// How sure the reader is that a skill was activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Certainty {
    /// The agent recorded a skill activation (Claude `Skill` tool, OpenCode
    /// skill message / tool).
    Certain,
    /// Deduced from a tool call reading `…/<name>/SKILL.md` (Codex).
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Text(String),
    ToolUse { name: String, input_summary: String },
    ToolResult { is_error: bool, summary: String },
    SkillActivation { name: String, certainty: Certainty },
}

/// One normalized transcript event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub ts: Option<SystemTime>,
    pub role: Role,
    pub kind: EventKind,
}

/// Something skipped or not understood; never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub path: PathBuf,
    pub message: String,
}

impl Warning {
    fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }
}

/// Which sessions to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Agents to include; empty = all.
    pub agents: Vec<AgentKind>,
    /// Only sessions whose cwd is this directory or inside it.
    pub project: Option<PathBuf>,
    /// Only sessions active at or after this time (unknown times are
    /// excluded).
    pub since: Option<SystemTime>,
    pub include_sidechains: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            project: None,
            since: None,
            include_sidechains: true,
        }
    }
}

impl Filter {
    fn wants(&self, agent: AgentKind) -> bool {
        self.agents.is_empty() || self.agents.contains(&agent)
    }

    /// Whether `info` passes the project / time / sidechain criteria.
    pub fn matches(&self, info: &SessionInfo) -> bool {
        if !self.wants(info.agent) || (info.sidechain && !self.include_sidechains) {
            return false;
        }
        if let Some(project) = &self.project
            && !info
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd.starts_with(project))
        {
            return false;
        }
        if let Some(since) = self.since
            && !info.ended.or(info.started).is_some_and(|t| t >= since)
        {
            return false;
        }
        true
    }
}

/// Caps applied while reading events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Bytes per text / summary; longer ones are cut and end with `…`.
    pub max_text: usize,
    /// Bytes of text over the whole transcript.
    pub max_total: usize,
    pub max_events: usize,
    /// Longer JSONL lines are skipped with a warning.
    pub max_line: usize,
    /// Pass every text through [`redact`].
    pub redact: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_text: 4 * 1024,
            max_total: 2 * 1024 * 1024,
            max_events: 20_000,
            max_line: 32 * 1024 * 1024,
            redact: true,
        }
    }
}

/// The events of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    pub events: Vec<Event>,
    /// Broken / oversized lines and unknown records.
    pub warnings: Vec<Warning>,
    /// A limit stopped reading before the end.
    pub truncated: bool,
}

#[derive(Debug)]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The OpenCode database could not be queried.
    Database { path: PathBuf, message: String },
    /// The OpenCode database has a schema this crate does not know.
    UnsupportedSchema { path: PathBuf, message: String },
    /// The reader for this agent is compiled out (cargo feature).
    Disabled(AgentKind),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Database { path, message } => write!(f, "{}: {message}", path.display()),
            Error::UnsupportedSchema { path, message } => {
                write!(f, "{}: unsupported schema: {message}", path.display())
            }
            Error::Disabled(agent) => write!(f, "{} reader is not compiled in", agent.id()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// List sessions of every agent at the default locations of `env`,
/// newest first.
pub fn list_sessions(env: &dyn Env, filter: &Filter) -> (Vec<SessionInfo>, Vec<Warning>) {
    list_sessions_in(&Roots::from_env(env), filter)
}

/// [`list_sessions`] over explicit locations.
pub fn list_sessions_in(roots: &Roots, filter: &Filter) -> (Vec<SessionInfo>, Vec<Warning>) {
    let mut sessions = Vec::new();
    let mut warnings = Vec::new();
    if filter.wants(AgentKind::Claude)
        && let Some(dir) = &roots.claude_projects
    {
        claude::list(dir, filter, &mut sessions, &mut warnings);
    }
    if filter.wants(AgentKind::Codex)
        && let Some(dir) = &roots.codex_sessions
    {
        codex::list(dir, filter, &mut sessions, &mut warnings);
    }
    if filter.wants(AgentKind::OpenCode)
        && let Some(db) = &roots.opencode_db
    {
        #[cfg(feature = "opencode")]
        opencode::list(db, &mut sessions, &mut warnings);
        #[cfg(not(feature = "opencode"))]
        if db.exists() {
            warnings.push(Warning::new(db, "OpenCode reader is not compiled in"));
        }
    }
    sessions.retain(|s| filter.matches(s));
    sessions.sort_by(|a, b| {
        b.ended
            .or(b.started)
            .cmp(&a.ended.or(a.started))
            .then_with(|| a.id.cmp(&b.id))
    });
    (sessions, warnings)
}

/// Stream the events of one session.
pub fn read_events(info: &SessionInfo, limits: &Limits) -> Result<Transcript> {
    read_with(info, limits, false)
}

/// Skill activations found by [`skill_activations`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activations {
    /// Each activation with its session, newest session first.
    pub found: Vec<(SessionInfo, Event)>,
    pub warnings: Vec<Warning>,
    /// A per-session or the global limit stopped collecting early.
    pub truncated: bool,
}

/// Every skill activation in the sessions matching `filter`.
///
/// `limits` applies to each session (`max_events` activations,
/// `max_total` bytes of names) and, with the same values, to the whole
/// result, so the output stays bounded however many sessions match.
pub fn skill_activations(env: &dyn Env, filter: &Filter, limits: &Limits) -> Activations {
    skill_activations_in(&Roots::from_env(env), filter, limits)
}

/// [`skill_activations`] over explicit locations.
pub fn skill_activations_in(roots: &Roots, filter: &Filter, limits: &Limits) -> Activations {
    let (sessions, warnings) = list_sessions_in(roots, filter);
    let mut out = Activations {
        warnings,
        ..Activations::default()
    };
    let mut bytes = 0usize;
    'sessions: for info in sessions {
        let transcript = match read_with(&info, limits, true) {
            Ok(t) => t,
            Err(err) => {
                out.warnings.push(Warning::new(&info.path, err.to_string()));
                continue;
            }
        };
        out.warnings.extend(transcript.warnings);
        out.truncated |= transcript.truncated;
        for event in transcript.events {
            let size = match &event.kind {
                EventKind::SkillActivation { name, .. } => name.len(),
                _ => 0,
            };
            if out.found.len() >= limits.max_events || bytes.saturating_add(size) > limits.max_total
            {
                out.truncated = true;
                break 'sessions;
            }
            bytes += size;
            out.found.push((info.clone(), event));
        }
    }
    out
}

fn read_with(info: &SessionInfo, limits: &Limits, only_skills: bool) -> Result<Transcript> {
    let mut sink = Sink::new(&info.path, limits, only_skills);
    match info.agent {
        AgentKind::Claude => claude::read(&info.path, &mut sink)?,
        AgentKind::Codex => codex::read(&info.path, &mut sink)?,
        #[cfg(feature = "opencode")]
        AgentKind::OpenCode => opencode::read(&info.path, &info.id, &mut sink)?,
        #[cfg(not(feature = "opencode"))]
        AgentKind::OpenCode => return Err(Error::Disabled(AgentKind::OpenCode)),
    }
    Ok(sink.finish())
}

/// Collects events under [`Limits`].
pub(crate) struct Sink<'a> {
    path: PathBuf,
    limits: &'a Limits,
    only_skills: bool,
    events: Vec<Event>,
    warnings: Vec<Warning>,
    bytes: usize,
    truncated: bool,
}

impl<'a> Sink<'a> {
    fn new(path: &Path, limits: &'a Limits, only_skills: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            limits,
            only_skills,
            events: Vec::new(),
            warnings: Vec::new(),
            bytes: 0,
            truncated: false,
        }
    }

    /// Reading should stop: a limit was hit.
    pub(crate) fn full(&self) -> bool {
        self.truncated
    }

    pub(crate) fn max_line(&self) -> usize {
        self.limits.max_line
    }

    pub(crate) fn warn(&mut self, message: impl Into<String>) {
        // Thousands of broken lines must not become thousands of warnings.
        if self.warnings.len() < 100 {
            self.warnings.push(Warning::new(&self.path, message));
        }
    }

    /// Prepare a text: redact the whole text first, then cap, so a cut
    /// can never separate a secret from the context that identifies it.
    pub(crate) fn text(&self, raw: &str) -> String {
        if self.limits.redact {
            cap(&redact(raw), self.limits.max_text)
        } else {
            cap(raw, self.limits.max_text)
        }
    }

    /// Prepare a JSON tool input / output: strings are redacted inside
    /// the decoded value (and values under credential-like keys masked)
    /// before serializing, then the result is capped.
    pub(crate) fn json(&self, value: &serde_json::Value) -> String {
        if self.limits.redact {
            cap(
                &summarize(&redact::redact_value(value)),
                self.limits.max_text,
            )
        } else {
            cap(&summarize(value), self.limits.max_text)
        }
    }

    /// A tool input that may be JSON text (Codex `arguments`): decoded and
    /// treated as [`Sink::json`] when it parses, else as plain text.
    pub(crate) fn json_or_text(&self, raw: &str) -> String {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => self.json(&v),
            _ => self.text(raw),
        }
    }

    pub(crate) fn push(&mut self, ts: Option<SystemTime>, role: Role, kind: EventKind) {
        if self.truncated {
            return;
        }
        if self.only_skills && !matches!(kind, EventKind::SkillActivation { .. }) {
            return;
        }
        let size = match &kind {
            EventKind::Text(t) => t.len(),
            EventKind::ToolUse {
                name,
                input_summary,
            } => name.len() + input_summary.len(),
            EventKind::ToolResult { summary, .. } => summary.len(),
            EventKind::SkillActivation { name, .. } => name.len(),
        };
        if self.events.len() >= self.limits.max_events
            || self.bytes.saturating_add(size) > self.limits.max_total
        {
            self.truncated = true;
            return;
        }
        self.bytes += size;
        self.events.push(Event { ts, role, kind });
    }

    fn finish(self) -> Transcript {
        Transcript {
            events: self.events,
            warnings: self.warnings,
            truncated: self.truncated,
        }
    }
}

/// The longest prefix of `s` of at most `max` bytes on a char boundary.
fn cap_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `s` cut to at most `max` bytes, with `…` appended when cut.
pub(crate) fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = cap_bytes(s, max.saturating_sub('…'.len_utf8())).to_string();
    out.push('…');
    out
}

/// A compact one-line summary of a JSON tool input or output.
pub(crate) fn summarize(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// File modification time; `None` when unreadable.
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_on_char_boundary() {
        assert_eq!(cap("abc", 3), "abc");
        assert_eq!(cap("abcdef", 5), "ab…");
        assert_eq!(cap("żżż", 4), "…");
    }
}
