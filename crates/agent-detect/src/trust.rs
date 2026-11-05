//! Workspace trust from `[agent.trust]` tables.
//!
//! Interactive agents ask once per folder whether the user trusts it. A host
//! that embeds the agent can check that state with [`workspace_trust`] and,
//! after asking its own user, record it with [`grant_workspace_trust`] so the
//! agent does not ask again inside the embedded terminal.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::host::Host;
use crate::registry::AgentDef;

/// Where and how an agent stores per-folder trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustKind {
    /// Claude Code: `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json`,
    /// `projects["<dir>"].hasTrustDialogAccepted = true`.
    ClaudeJson,
    /// Codex: `$CODEX_HOME/config.toml` or `~/.codex/config.toml`,
    /// `[projects."<dir>"] trust_level = "trusted"`.
    CodexToml,
}

/// An `[agent.trust]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TrustDef {
    pub kind: TrustKind,
}

/// Trust state of one folder for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    Trusted,
    /// The agent will ask on start.
    Untrusted,
    /// The agent has no trust concept, or its file cannot be read or parsed.
    Unknown,
}

/// Why trust could not be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    /// The agent has no `[agent.trust]` table.
    NotSupported,
    /// The trust file is not valid JSON/TOML or has an unexpected shape; it
    /// was left untouched.
    Parse(String),
    /// Filesystem failure (also: the folder does not exist).
    Io(String),
    /// The trust file kept changing on disk while being updated.
    Conflict,
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustError::NotSupported => write!(f, "agent has no workspace trust"),
            TrustError::Parse(msg) => write!(f, "cannot parse trust file: {msg}"),
            TrustError::Io(msg) => write!(f, "trust file I/O error: {msg}"),
            TrustError::Conflict => write!(f, "trust file changed concurrently"),
        }
    }
}

impl std::error::Error for TrustError {}

/// Home directory and environment variables, injectable for tests.
///
/// Every [`Host`] is a `TrustEnv`.
pub trait TrustEnv {
    fn home(&self) -> PathBuf;
    /// Value of an environment variable; an empty value counts as unset.
    fn var(&self, key: &str) -> Option<String>;
}

impl<H: Host + ?Sized> TrustEnv for H {
    fn home(&self) -> PathBuf {
        Host::home(self)
    }

    fn var(&self, key: &str) -> Option<String> {
        Host::var(self, key)
    }
}

/// The file holding `def`'s trust records; `None` without a trust table.
pub fn trust_file(def: &AgentDef, env: &dyn TrustEnv) -> Option<PathBuf> {
    let kind = def.trust.as_ref()?.kind;
    let non_empty = |key: &str| env.var(key).filter(|v| !v.is_empty());
    Some(match kind {
        TrustKind::ClaudeJson => match non_empty("CLAUDE_CONFIG_DIR") {
            Some(dir) => PathBuf::from(dir).join(".claude.json"),
            None => env.home().join(".claude.json"),
        },
        TrustKind::CodexToml => match non_empty("CODEX_HOME") {
            Some(dir) => PathBuf::from(dir).join("config.toml"),
            None => env.home().join(".codex").join("config.toml"),
        },
    })
}

/// Whether `def` trusts `dir`. An entry for `dir` as given or for its
/// realpath counts; a missing trust file means untrusted.
pub fn workspace_trust(def: &AgentDef, env: &dyn TrustEnv, dir: &Path) -> TrustState {
    let (Some(kind), Some(file)) = (def.trust.as_ref().map(|t| t.kind), trust_file(def, env))
    else {
        return TrustState::Unknown;
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return TrustState::Untrusted,
        Err(_) => return TrustState::Unknown,
    };
    let mut keys: Vec<String> = dir.to_str().map(str::to_string).into_iter().collect();
    if let Some(real) = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        && !keys.contains(&real)
    {
        keys.push(real);
    }
    let trusted = match kind {
        TrustKind::ClaudeJson => {
            let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
                return TrustState::Unknown;
            };
            keys.iter().any(|k| {
                root.get("projects")
                    .and_then(|p| p.get(k))
                    .and_then(|p| p.get("hasTrustDialogAccepted"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            })
        }
        TrustKind::CodexToml => {
            let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
                return TrustState::Unknown;
            };
            keys.iter().any(|k| {
                doc.get("projects")
                    .and_then(|p| p.get(k))
                    .and_then(|p| p.get("trust_level"))
                    .and_then(toml_edit::Item::as_str)
                    == Some("trusted")
            })
        }
    };
    if trusted {
        TrustState::Trusted
    } else {
        TrustState::Untrusted
    }
}

/// Record that `def` trusts `dir` (stored under its realpath).
///
/// Read-modify-write that changes only the one key: the file is written to
/// a temp file next to it with the original permissions and renamed over
/// it. If the file changed on disk meanwhile the update is retried (3
/// attempts). A file that does not parse is never written. A missing file is
/// created.
pub fn grant_workspace_trust(
    def: &AgentDef,
    env: &dyn TrustEnv,
    dir: &Path,
) -> Result<(), TrustError> {
    let (Some(kind), Some(file)) = (def.trust.as_ref().map(|t| t.kind), trust_file(def, env))
    else {
        return Err(TrustError::NotSupported);
    };
    let real = dir
        .canonicalize()
        .map_err(|err| TrustError::Io(format!("{}: {err}", dir.display())))?;
    let key = real
        .to_str()
        .ok_or_else(|| TrustError::Io(format!("{}: not valid UTF-8", real.display())))?;
    // Write through a symlinked config file to its target.
    let file = file.canonicalize().unwrap_or(file);
    for _ in 0..3 {
        let before = snapshot(&file)?;
        let text = match &before {
            Some(_) => Some(std::fs::read_to_string(&file).map_err(io_err(&file))?),
            None => None,
        };
        let updated = match kind {
            TrustKind::ClaudeJson => set_claude(text.as_deref(), key)?,
            TrustKind::CodexToml => set_codex(text.as_deref(), key)?,
        };
        if write_atomically(&file, &updated, &before)? {
            return Ok(());
        }
    }
    Err(TrustError::Conflict)
}

fn set_claude(text: Option<&str>, key: &str) -> Result<String, TrustError> {
    use serde_json::{Map, Value};
    let mut root = match text {
        Some(text) => {
            serde_json::from_str::<Value>(text).map_err(|err| TrustError::Parse(err.to_string()))?
        }
        None => Value::Object(Map::new()),
    };
    let shape = |what: &str| TrustError::Parse(format!("{what} is not a JSON object"));
    let projects = root
        .as_object_mut()
        .ok_or_else(|| shape("root"))?
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| shape("projects"))?;
    projects
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| shape("project entry"))?
        .insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    let mut out =
        serde_json::to_string_pretty(&root).map_err(|err| TrustError::Parse(err.to_string()))?;
    if text.is_none_or(|t| t.ends_with('\n')) {
        out.push('\n');
    }
    Ok(out)
}

fn set_codex(text: Option<&str>, key: &str) -> Result<String, TrustError> {
    use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};
    let mut doc = text
        .unwrap_or("")
        .parse::<DocumentMut>()
        .map_err(|err| TrustError::Parse(err.to_string()))?;
    let shape = |what: &str| TrustError::Parse(format!("{what} is not a TOML table"));
    let projects = doc.entry("projects").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let project = match projects {
        Item::Table(table) => table
            .entry(key)
            .or_insert_with(|| Item::Table(Table::new())),
        Item::Value(Value::InlineTable(table)) => {
            match table.entry(key).or_insert(InlineTable::new().into()) {
                Value::InlineTable(project) => {
                    project.insert("trust_level", "trusted".into());
                    return Ok(doc.to_string());
                }
                _ => return Err(shape("project entry")),
            }
        }
        _ => return Err(shape("projects")),
    };
    project
        .as_table_like_mut()
        .ok_or_else(|| shape("project entry"))?
        .insert("trust_level", value("trusted"));
    Ok(doc.to_string())
}

/// mtime, length and mode of `file`; `None` when it does not exist.
type Snapshot = Option<(SystemTime, u64, u32)>;

fn snapshot(file: &Path) -> Result<Snapshot, TrustError> {
    match std::fs::metadata(file) {
        Ok(meta) => Ok(Some((
            meta.modified().map_err(io_err(file))?,
            meta.len(),
            meta.permissions().mode(),
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_err(file)(err)),
    }
}

/// Write `contents` to a temp file beside `file` and rename it over `file`,
/// unless `file` changed since `before` (then `Ok(false)`, nothing written).
fn write_atomically(file: &Path, contents: &str, before: &Snapshot) -> Result<bool, TrustError> {
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (tmp, mut handle) = create_temp(parent, &name)?;
    let result = (|| {
        handle
            .write_all(contents.as_bytes())
            .and_then(|()| handle.sync_all())
            .map_err(io_err(&tmp))?;
        // Keep the original mode; a new file is private (it may hold tokens).
        let mode = before.map_or(0o600, |(_, _, mode)| mode & 0o7777);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(io_err(&tmp))?;
        if snapshot(file)? != *before {
            return Ok(false);
        }
        std::fs::rename(&tmp, file).map_err(io_err(file))?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn create_temp(parent: &Path, name: &str) -> Result<(PathBuf, std::fs::File), TrustError> {
    let pid = std::process::id();
    let mut last = None;
    for n in 0..100u32 {
        let tmp = parent.join(format!(".{name}.trust-{pid}-{n}.tmp"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(handle) => return Ok((tmp, handle)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => last = Some(err),
            Err(err) => return Err(io_err(&tmp)(err)),
        }
    }
    Err(TrustError::Io(format!(
        "{}: no free temp file name: {last:?}",
        parent.display()
    )))
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> TrustError + '_ {
    move |err| TrustError::Io(format!("{}: {err}", path.display()))
}
