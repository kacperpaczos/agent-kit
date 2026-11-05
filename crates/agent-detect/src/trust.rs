//! Workspace trust from `[agent.trust]` tables.
//!
//! Interactive agents ask once per folder whether the user trusts it. A host
//! that embeds the agent can check that state with [`workspace_trust`] and,
//! after asking its own user, record it with [`grant_workspace_trust`] so the
//! agent does not ask again inside the embedded terminal.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
/// Read-modify-write that changes only the one key and keeps the rest of
/// the file (Codex: formatting and comments too). An entry that is already
/// trusted is left alone and the file is not written. A file that does not
/// parse is never written. A missing file is created (mode 0600).
///
/// The new content goes to a private (0600) temp file next to the trust
/// file, gets the original permissions, is fsynced and renamed over it. A
/// symlinked trust file is written through to its target; a dangling
/// symlink is an error and nothing is written. Right before the rename the
/// file is read again and compared with what was parsed; if it differs, the
/// whole read-modify-write is redone (3 attempts, then
/// [`TrustError::Conflict`]).
///
/// Remaining race: the agent does not lock its file, so a write by the
/// agent that lands between that final re-read and the rename is lost. The
/// window is a few syscalls wide.
pub fn grant_workspace_trust(
    def: &AgentDef,
    env: &dyn TrustEnv,
    dir: &Path,
) -> Result<(), TrustError> {
    grant(def, env, dir, &mut |_| {})
}

/// Points inside [`grant_workspace_trust`] where a test can observe or
/// interfere with the write.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub enum TrustWriteStage<'a> {
    /// The temp file was just created, before anything is written to it.
    TempCreated { tmp: &'a Path },
    /// The temp file is complete; the trust file is re-read and compared
    /// next, then renamed over. `attempt` counts from 0.
    BeforeRename {
        attempt: u32,
        file: &'a Path,
        tmp: &'a Path,
    },
}

/// [`grant_workspace_trust`] with a hook called at each [`TrustWriteStage`].
#[cfg(any(test, feature = "test-support"))]
pub fn grant_workspace_trust_hooked(
    def: &AgentDef,
    env: &dyn TrustEnv,
    dir: &Path,
    hook: &mut dyn FnMut(TrustWriteStage<'_>),
) -> Result<(), TrustError> {
    grant(def, env, dir, &mut |stage| hook(stage.into()))
}

/// Internal mirror of `TrustWriteStage`, present in every build (its
/// fields are only read by the test hook).
#[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
enum Stage<'a> {
    TempCreated(&'a Path),
    BeforeRename(u32, &'a Path, &'a Path),
}

#[cfg(any(test, feature = "test-support"))]
impl<'a> From<Stage<'a>> for TrustWriteStage<'a> {
    fn from(stage: Stage<'a>) -> Self {
        match stage {
            Stage::TempCreated(tmp) => TrustWriteStage::TempCreated { tmp },
            Stage::BeforeRename(attempt, file, tmp) => {
                TrustWriteStage::BeforeRename { attempt, file, tmp }
            }
        }
    }
}

const ATTEMPTS: u32 = 3;

fn grant(
    def: &AgentDef,
    env: &dyn TrustEnv,
    dir: &Path,
    hook: &mut dyn FnMut(Stage<'_>),
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
    let file = write_target(&file)?;
    for attempt in 0..ATTEMPTS {
        let text = read_optional(&file)?;
        let updated = match kind {
            TrustKind::ClaudeJson => set_claude(text.as_deref(), key)?,
            TrustKind::CodexToml => set_codex(text.as_deref(), key)?,
        };
        let Some(updated) = updated else {
            return Ok(());
        };
        if write_atomically(&file, &updated, text.as_deref(), attempt, hook)? {
            return Ok(());
        }
    }
    Err(TrustError::Conflict)
}

/// The path to replace: `file` itself, or the target of a symlinked `file`.
/// A dangling symlink is an error (writing would replace the link).
fn write_target(file: &Path) -> Result<PathBuf, TrustError> {
    match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.file_type().is_symlink() => file
            .canonicalize()
            .map_err(|err| TrustError::Io(format!("{}: dangling symlink: {err}", file.display()))),
        Ok(_) => Ok(file.to_path_buf()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(file.to_path_buf()),
        Err(err) => Err(io_err(file)(err)),
    }
}

/// Contents of `file`; `None` when it does not exist.
fn read_optional(file: &Path) -> Result<Option<String>, TrustError> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_err(file)(err)),
    }
}

/// New file text, or `None` when the entry is already trusted.
fn set_claude(text: Option<&str>, key: &str) -> Result<Option<String>, TrustError> {
    use serde_json::{Map, Value};
    let mut root = match text {
        Some(text) => {
            serde_json::from_str::<Value>(text).map_err(|err| TrustError::Parse(err.to_string()))?
        }
        None => Value::Object(Map::new()),
    };
    let shape = |what: &str| TrustError::Parse(format!("{what} is not a JSON object"));
    let project = root
        .as_object_mut()
        .ok_or_else(|| shape("root"))?
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| shape("projects"))?
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| shape("project entry"))?;
    if project.get("hasTrustDialogAccepted") == Some(&Value::Bool(true)) {
        return Ok(None);
    }
    project.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    let mut out =
        serde_json::to_string_pretty(&root).map_err(|err| TrustError::Parse(err.to_string()))?;
    if text.is_none_or(|t| t.ends_with('\n')) {
        out.push('\n');
    }
    Ok(Some(out))
}

/// New file text, or `None` when the entry is already trusted. An existing
/// `trust_level` keeps its key and value decoration (spacing, comments).
fn set_codex(text: Option<&str>, key: &str) -> Result<Option<String>, TrustError> {
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
    let inline = projects.is_inline_table();
    let projects = projects
        .as_table_like_mut()
        .ok_or_else(|| shape("projects"))?;
    if projects.get(key).is_none() {
        let entry = if inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            Item::Table(Table::new())
        };
        projects.insert(key, entry);
    }
    let project = projects
        .get_mut(key)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| shape("project entry"))?;
    match project.get_mut("trust_level") {
        Some(Item::Value(level)) if level.as_str() == Some("trusted") => return Ok(None),
        Some(Item::Value(level)) => {
            let decor = level.decor().clone();
            *level = Value::from("trusted");
            *level.decor_mut() = decor;
        }
        _ => {
            project.insert("trust_level", value("trusted"));
        }
    }
    Ok(Some(doc.to_string()))
}

/// Write `contents` to a private temp file beside `file`, give it the
/// original mode, fsync, then rename it over `file` unless `file` no longer
/// holds `read_text` (then `Ok(false)`, nothing replaced).
fn write_atomically(
    file: &Path,
    contents: &str,
    read_text: Option<&str>,
    attempt: u32,
    hook: &mut dyn FnMut(Stage<'_>),
) -> Result<bool, TrustError> {
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Keep the original mode; a new file is private (it may hold tokens).
    let mode = match std::fs::metadata(file) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(err) => return Err(io_err(file)(err)),
    };
    let (tmp, mut handle) = create_temp(parent, &name)?;
    let result = (|| {
        hook(Stage::TempCreated(&tmp));
        handle
            .write_all(contents.as_bytes())
            .and_then(|()| handle.set_permissions(std::fs::Permissions::from_mode(mode)))
            .and_then(|()| handle.sync_all())
            .map_err(io_err(&tmp))?;
        hook(Stage::BeforeRename(attempt, file, &tmp));
        if read_optional(file)?.as_deref() != read_text {
            return Ok(false);
        }
        std::fs::rename(&tmp, file).map_err(io_err(file))?;
        // Best effort: make the rename itself durable.
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A new temp file beside the target, created with mode 0600.
fn create_temp(parent: &Path, name: &str) -> Result<(PathBuf, std::fs::File), TrustError> {
    let pid = std::process::id();
    let mut last = None;
    for n in 0..100u32 {
        let tmp = parent.join(format!(".{name}.trust-{pid}-{n}.tmp"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
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
