//! Shared plumbing for the adapters: an argv [`Plan`], one CLI behind the
//! injected runner, and the error/warning shapes every adapter reports.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::runner::{CancelToken, Command, CommandRunner, Output, RunError};
use crate::types::{MarketplaceError, Outcome, Result, Scope};

pub(crate) const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const LIST_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(crate) const MUTATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Argv after the adapter's fixed prefix (e.g. `npx --yes skills`), plus
/// the directory to run in (the project for project scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl Plan {
    /// `sub` as the first argument; runs in the project for project scope.
    pub(crate) fn new(sub: &str, scope: Option<&Scope>) -> Plan {
        Plan {
            args: vec![sub.to_string()],
            cwd: match scope {
                Some(Scope::Project(dir)) => Some(dir.clone()),
                _ => None,
            },
        }
    }
    pub(crate) fn push(&mut self, arg: &str) {
        self.args.push(arg.to_string());
    }
    pub(crate) fn extend<'a>(&mut self, args: impl IntoIterator<Item = &'a str>) {
        self.args.extend(args.into_iter().map(str::to_string));
    }
    pub(crate) fn in_dir(mut self, dir: Option<&Path>) -> Plan {
        self.cwd = dir.map(Path::to_path_buf);
        self
    }
}

/// One CLI: `<program> <prefix…> <plan args…>` through the runner, sharing
/// one cancel token.
pub(crate) struct Cli<R> {
    pub runner: R,
    pub program: String,
    pub prefix: Vec<String>,
    pub cancel: CancelToken,
}

impl<R: CommandRunner> Cli<R> {
    pub fn new(runner: R, program: &str, prefix: &[&str]) -> Cli<R> {
        Cli {
            runner,
            program: program.to_string(),
            prefix: prefix.iter().map(|s| s.to_string()).collect(),
            cancel: CancelToken::new(),
        }
    }

    /// Run; a non-zero exit is still `Ok`.
    pub fn run(&self, plan: Plan, timeout: Duration) -> Result<Output> {
        let mut args = self.prefix.clone();
        args.extend(plan.args);
        let cmd = Command {
            program: self.program.clone(),
            args,
            cwd: plan.cwd,
            env: vec![],
            timeout,
            cancel: self.cancel.clone(),
        };
        self.runner.run(&cmd).map_err(|e| match e {
            RunError::Cancelled => MarketplaceError::Cancelled,
            other => MarketplaceError::Run(other.to_string()),
        })
    }

    /// Run and require exit code 0.
    pub fn run_ok(&self, plan: Plan, timeout: Duration) -> Result<Output> {
        let output = self.run(plan, timeout)?;
        if output.success() {
            Ok(output)
        } else {
            Err(failed(&output))
        }
    }

    /// Human label for warnings, e.g. `skillfish update`.
    pub fn label(&self, sub: &str) -> String {
        let tool = self
            .prefix
            .iter()
            .rev()
            .find(|a| !a.starts_with('-'))
            .unwrap_or(&self.program);
        format!("{tool} {sub}")
    }
}

pub(crate) fn failed(output: &Output) -> MarketplaceError {
    failed_with(output, output.stderr.trim())
}

/// A failure whose message comes from somewhere other than stderr (JSON
/// error fields); falls back to stderr when `message` is empty.
pub(crate) fn failed_with(output: &Output, message: &str) -> MarketplaceError {
    let message = if message.is_empty() {
        output.stderr.trim()
    } else {
        message
    };
    MarketplaceError::Failed {
        status: output.status,
        stderr: message.to_string(),
    }
}

pub(crate) fn exit_warning(command: &str, output: &Output) -> String {
    let code = output
        .status
        .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        format!("`{command}` exited with {code}")
    } else {
        format!("`{command}` exited with {code}: {stderr}")
    }
}

/// Push `item` unless already present (keeps first-seen order).
pub(crate) fn push_unique(list: &mut Vec<String>, item: &str) {
    if !list.iter().any(|x| x == item) {
        list.push(item.to_string());
    }
}

/// Every word of `query` (lower-cased) occurs in one of `fields`.
pub(crate) fn matches_query(query: &[String], fields: &[Option<&str>]) -> bool {
    let haystack: Vec<String> = fields.iter().flatten().map(|f| f.to_lowercase()).collect();
    query
        .iter()
        .all(|word| haystack.iter().any(|h| h.contains(word.as_str())))
}

/// Lower-cased words of a search query; `Invalid` when there are none.
pub(crate) fn query_words(query: &str) -> Result<Vec<String>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Err(MarketplaceError::Invalid("empty search query".to_string()));
    }
    Ok(words)
}

/// Run `step` for every item, keeping what earlier steps confirmed: a
/// failed step becomes a warning and the rest still run (cancellation
/// stops the loop). `Err` only when nothing at all was confirmed.
pub(crate) fn run_each<T>(
    items: impl IntoIterator<Item = T>,
    outcome: &mut Outcome,
    mut step: impl FnMut(T, &mut Outcome) -> Result<()>,
) -> Result<()> {
    let mut first: Option<MarketplaceError> = None;
    for item in items {
        match step(item, outcome) {
            Ok(()) => {}
            Err(MarketplaceError::Cancelled) => {
                outcome.warnings.push("cancelled".to_string());
                first.get_or_insert(MarketplaceError::Cancelled);
                break;
            }
            Err(e) => {
                outcome.warnings.push(e.to_string());
                first.get_or_insert(e);
            }
        }
    }
    match first {
        Some(e) if outcome.changed.is_empty() => Err(e),
        _ => Ok(()),
    }
}
