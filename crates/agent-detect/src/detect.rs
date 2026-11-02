//! Effective PATH computation and agent detection (spec §7.1–7.2).

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;

use crate::host::{Host, SpawnError};
use crate::paths;
use crate::registry::AgentDef;
use crate::status::{AgentState, AgentStatus};

/// Timeout for `version_args` and auth-probe runs.
const RUN_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for the login-shell PATH probe.
const LOGIN_PATH_TIMEOUT: Duration = Duration::from_secs(3);
/// Version pattern used when the agent defines no `version_regex`.
const DEFAULT_VERSION_PATTERN: &str = r"\d+\.\d+(\.\d+)?";

/// Well-known per-user bin directories, always part of the effective PATH.
const KNOWN_HOME_BINS: &[&str] = &[
    ".local/bin",
    ".cargo/bin",
    ".bun/bin",
    ".npm-global/bin",
    ".volta/bin",
    ".local/share/mise/shims",
    ".local/share/fnm/aliases/default/bin",
];

/// The directories binaries are resolved in and child processes inherit as
/// PATH: the process `PATH`, the login-shell `PATH`, and well-known user bin
/// directories, deduplicated preserving first occurrence.
pub fn effective_path(host: &dyn Host) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path_var) = host.var("PATH") {
        dirs.extend(
            path_var
                .split(':')
                // Empty components (trailing/duplicate ':') are not directories.
                .filter(|part| !part.is_empty())
                .map(PathBuf::from),
        );
    }
    if let Some(login) = host.login_shell_path(LOGIN_PATH_TIMEOUT) {
        dirs.extend(login);
    }
    let home = host.home();
    for rel in KNOWN_HOME_BINS {
        dirs.push(home.join(rel));
    }
    dirs.extend(nvm_bins(&home));
    dirs.push(home.join(".opencode/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));
    dedupe(dirs)
}

/// Every `~/.nvm/versions/node/*/bin`, lexicographically greatest version
/// first (glob semantics: only version dirs that contain `bin`).
fn nvm_bins(home: &Path) -> Vec<PathBuf> {
    let versions = home.join(".nvm/versions/node");
    let entries = match std::fs::read_dir(&versions) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<std::ffi::OsString> = entries
        .flatten()
        .filter(|entry| entry.path().join("bin").is_dir())
        .map(|entry| entry.file_name())
        .collect();
    names.sort_unstable();
    names.reverse();
    names
        .into_iter()
        .map(|name| versions.join(name).join("bin"))
        .collect()
}

fn dedupe(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(dirs.len());
    let mut unique: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if seen.insert(dir.clone()) {
            unique.push(dir);
        }
    }
    unique
}

/// Detect every listed agent, in list order, against one shared effective
/// PATH.
pub fn detect_all(defs: &[AgentDef], host: &dyn Host) -> Vec<AgentStatus> {
    let path = effective_path(host);
    defs.iter()
        .map(|def| detect_agent(def, host, &path))
        .collect()
}

/// Detect a single agent's lifecycle state (spec §7.2).
pub fn detect_agent(def: &AgentDef, host: &dyn Host, path: &[PathBuf]) -> AgentStatus {
    let config_dir = first_existing_config_dir(def, host);
    let mut diagnostics: Vec<String> = Vec::new();
    // The last candidate actually attempted; the binary a `BinaryOnly` state
    // reports when nothing ever produced a version.
    let mut attempted: Option<PathBuf> = None;
    for candidate in candidates(def, host, path) {
        attempted = Some(candidate.clone());
        match host.run(&candidate, &def.version_args, path, RUN_TIMEOUT) {
            // Uninvocable binary (e.g. an abandoned npm shim): diagnose and
            // try the next candidate.
            Err(
                err @ (SpawnError::NotFound | SpawnError::PermissionDenied | SpawnError::Other(_)),
            ) => {
                diagnostics.push(format!(
                    "{}: not invocable ({})",
                    candidate.display(),
                    spawn_error_text(&err)
                ));
            }
            // A timeout or a failing exit means the agent is broken here;
            // do not keep trying further candidates.
            Err(SpawnError::Timeout) => {
                diagnostics.push(format!("{}: timed out", candidate.display()));
                break;
            }
            Ok(output) if output.status != Some(0) => {
                diagnostics.push(exit_status_diagnostic(&candidate, output.status));
                break;
            }
            Ok(output) => match parse_version(def, &output.stdout) {
                Some(version) => {
                    return with_version(
                        def,
                        host,
                        path,
                        &candidate,
                        version,
                        config_dir.as_deref(),
                        diagnostics,
                    );
                }
                None => {
                    diagnostics.push("unparseable version".to_string());
                    break;
                }
            },
        }
    }
    // No working binary: an existing config outranks a version-less binary,
    // and a version-less binary outranks nothing at all.
    let state = match (&config_dir, &attempted) {
        (Some(config_dir), _) => AgentState::ConfigOnly {
            config_dir: config_dir.clone(),
        },
        (None, Some(bin)) => AgentState::BinaryOnly { bin: bin.clone() },
        (None, None) => AgentState::Absent,
    };
    AgentStatus {
        agent_id: def.id.clone(),
        state,
        diagnostics,
        searched_dirs: path.to_vec(),
    }
}

/// With a parsed version: `Ready` when the auth probe passes (or is undefined
/// and a config dir exists), otherwise `Runnable`.
fn with_version(
    def: &AgentDef,
    host: &dyn Host,
    path: &[PathBuf],
    bin: &Path,
    version: String,
    config_dir: Option<&Path>,
    mut diagnostics: Vec<String>,
) -> AgentStatus {
    let state = if let Some(probe) = &def.auth_probe {
        let passed = matches!(host.run(bin, probe, path, RUN_TIMEOUT), Ok(output) if output.status == Some(0));
        if passed {
            AgentState::Ready {
                bin: bin.to_path_buf(),
                version,
            }
        } else {
            diagnostics.push("auth probe failed".to_string());
            AgentState::Runnable {
                bin: bin.to_path_buf(),
                version,
            }
        }
    } else if config_dir.is_some() {
        AgentState::Ready {
            bin: bin.to_path_buf(),
            version,
        }
    } else {
        AgentState::Runnable {
            bin: bin.to_path_buf(),
            version,
        }
    };
    AgentStatus {
        agent_id: def.id.clone(),
        state,
        diagnostics,
        searched_dirs: path.to_vec(),
    }
}

/// Binary candidates in priority order: the `bin_env` override alone when set,
/// else every existing executable `bin` in `path` order, then the fallbacks.
fn candidates(def: &AgentDef, host: &dyn Host, path: &[PathBuf]) -> Vec<PathBuf> {
    if let Some(var) = &def.bin_env
        && let Some(value) = host.var(var)
    {
        return vec![PathBuf::from(value)];
    }
    let mut names: Vec<String> = Vec::with_capacity(1 + def.fallback_bins.len());
    names.push(def.bin.clone());
    names.extend(def.fallback_bins.iter().cloned());
    let mut found = Vec::new();
    for name in names {
        for dir in path {
            let candidate = dir.join(&name);
            if is_executable_file(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

/// First `config_dirs` template that expands to an existing directory.
fn first_existing_config_dir(def: &AgentDef, host: &dyn Host) -> Option<PathBuf> {
    def.config_dirs.iter().find_map(|template| {
        let dir = paths::expand(template, host)?;
        dir.exists().then_some(dir)
    })
}

/// A regular file with at least one execute bit set.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// Version from `--version` output: the first capture group of
/// `version_regex` when set (falling back to its whole match), else the first
/// match of [`DEFAULT_VERSION_PATTERN`].
fn parse_version(def: &AgentDef, stdout: &str) -> Option<String> {
    match def.version_regex.as_deref() {
        Some(pattern) => {
            let regex = Regex::new(pattern).ok()?;
            regex
                .captures(stdout)
                .and_then(|caps| caps.get(1).or_else(|| caps.get(0)))
                .map(|m| m.as_str().to_string())
        }
        None => Regex::new(DEFAULT_VERSION_PATTERN)
            .ok()?
            .find(stdout)
            .map(|m| m.as_str().to_string()),
    }
}

fn spawn_error_text(err: &SpawnError) -> String {
    match err {
        SpawnError::NotFound => "not found".to_string(),
        SpawnError::PermissionDenied => "permission denied".to_string(),
        // Unreachable from the caller (a timeout stops the search instead),
        // listed to keep the match exhaustive.
        SpawnError::Timeout => "timeout".to_string(),
        SpawnError::Other(msg) => msg.clone(),
    }
}

fn exit_status_diagnostic(bin: &Path, status: Option<i32>) -> String {
    match status {
        Some(code) => format!("{}: exited with status {code}", bin.display()),
        None => format!("{}: exited without status", bin.display()),
    }
}
