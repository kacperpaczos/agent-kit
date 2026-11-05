//! Adapter for skillfish (`npx skillfish`, knoxgraeme/skillfish).
//!
//! Every command runs with the global `--json` flag, which also makes
//! skillfish non-interactive. Facts from its README and `src/commands`:
//!
//! - `search <query> [--limit n]` (1–50) queries skill.fish.
//! - `add <owner/repo> [skill] [--all] [--path p] [--force] [--yes]
//!   [--global|--project] [--agent <Display Name>]...`; one skill name per
//!   call.
//! - `remove [skill|--all] [--yes] [--global|--project] [--agent <name>]`;
//!   one skill and at most one agent per call.
//! - `list [--global|--project]` shows every skill in agent directories.
//! - `update --json` only checks; `update --yes --json` updates every
//!   outdated tracked skill (no per-skill selection, no location flags;
//!   project skills are those under the working directory).
//!
//! skillfish owns its `.skillfish.json` manifests; this adapter only reads
//! them.

mod parse;

use std::path::{Path, PathBuf};

pub use parse::{OutdatedSkill, Row, UpdateReport, parse_list, parse_search, parse_update};

use crate::exec::{
    CHECK_TIMEOUT, Cli, LIST_TIMEOUT, MUTATE_TIMEOUT, Plan, SEARCH_TIMEOUT, exit_warning, failed,
    failed_with, push_unique,
};
use crate::npx_skills::AGENT_NAMES;
use crate::provenance::read_skillfish_manifest;
use crate::runner::{CancelToken, CommandRunner, Output};
use crate::types::{
    AgentSelection, Capabilities, ChannelId, InstallRequest, InstalledSkill, Marketplace,
    MarketplaceError, Outcome, RemoveRequest, Result, Scope, SearchResults, SourceKind, UpdateInfo,
    UpdateRequest,
};
use crate::validate::{Kind, check, check_name_or_all};

/// Results requested from skill.fish (upstream allows 1–50).
pub const SEARCH_LIMIT: u32 = 20;

/// skillfish agent name (`--agent`) for an agent-kit id; skillfish uses
/// the same display names as `npx skills list` (`Claude Code`, `Codex`,
/// `OpenCode`).
pub fn skillfish_agent_name(id: &str) -> Option<&'static str> {
    AGENT_NAMES.iter().find(|a| a.id == id).map(|a| a.display)
}

fn json_plan(sub: &str, scope: Option<&Scope>) -> Plan {
    let mut plan = Plan::new("--json", scope);
    plan.push(sub);
    plan
}

fn location_flag(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "--global",
        Scope::Project(_) => "--project",
    }
}

/// Where `update` runs: the project, or HOME for global scope so that no
/// unrelated project under the caller's directory is checked or updated.
fn update_dir(scope: &Scope, home: &Path) -> PathBuf {
    match scope {
        Scope::Global => home.to_path_buf(),
        Scope::Project(dir) => dir.clone(),
    }
}

/// `--json search <query> --limit 20`; the words go back together as one
/// argument.
pub fn plan_search(query: &str) -> Result<Plan> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Err(MarketplaceError::Invalid("empty search query".to_string()));
    }
    words.iter().try_for_each(|w| check(Kind::Word, w))?;
    let mut plan = json_plan("search", None);
    plan.push(&words.join(" "));
    plan.extend(["--limit", &SEARCH_LIMIT.to_string()]);
    Ok(plan)
}

/// `--json list --global|--project`.
pub fn plan_list(scope: &Scope) -> Plan {
    let mut plan = json_plan("list", Some(scope));
    plan.push(location_flag(scope));
    plan
}

/// `--json update`: check only.
pub fn plan_check(scope: &Scope, home: &Path) -> Plan {
    json_plan("update", None).in_dir(Some(&update_dir(scope, home)))
}

/// `--json update --yes`: update every outdated tracked skill.
pub fn plan_update_all(scope: &Scope, home: &Path) -> Plan {
    let mut plan = plan_check(scope, home);
    plan.push("--yes");
    plan
}

/// `--agent <Display Name>` values; ids map to display names, other names
/// pass through. `Detected` and `All` pass none (skillfish then targets
/// every detected agent).
fn agent_names(agents: &AgentSelection) -> Result<Vec<String>> {
    match agents {
        AgentSelection::Detected | AgentSelection::All => Ok(vec![]),
        AgentSelection::Only(list) if list.is_empty() => {
            Err(MarketplaceError::Invalid("empty agent list".to_string()))
        }
        AgentSelection::Only(list) => list
            .iter()
            .map(|a| {
                check(Kind::Name, a)?;
                Ok(skillfish_agent_name(a).unwrap_or(a).to_string())
            })
            .collect(),
    }
}

/// Requested names: `*` alone or plain names.
fn checked_names(names: &[String]) -> Result<bool> {
    let all = names.iter().any(|s| s == "*");
    if all && names.len() > 1 {
        return Err(MarketplaceError::Invalid(
            "'*' cannot be combined with skill names".to_string(),
        ));
    }
    names.iter().try_for_each(|s| check_name_or_all(s))?;
    Ok(all)
}

/// One `--json add <owner/repo> [skill|--all] --yes --global|--project
/// [--agent <name>]...` per requested skill. GitHub sources only.
pub fn plan_install(req: &InstallRequest) -> Result<Vec<Plan>> {
    let repo = match &req.source.kind {
        SourceKind::Github { owner, repo } => {
            check(Kind::Name, owner)?;
            check(Kind::Name, repo)?;
            format!("{owner}/{repo}")
        }
        _ => {
            return Err(MarketplaceError::Unsupported(
                "skillfish installs from GitHub repositories only".to_string(),
            ));
        }
    };
    let skills: Vec<String> = if req.skills.is_empty() {
        req.source.skill.iter().cloned().collect()
    } else {
        req.skills.clone()
    };
    let all = checked_names(&skills)?;
    let agents = agent_names(&req.agents)?;
    let one = |selector: Option<&str>| {
        let mut plan = json_plan("add", Some(&req.scope));
        plan.push(&repo);
        if let Some(selector) = selector {
            plan.push(selector);
        }
        plan.extend(["--yes", location_flag(&req.scope)]);
        for agent in &agents {
            plan.extend(["--agent", agent]);
        }
        plan
    };
    Ok(if all {
        vec![one(Some("--all"))]
    } else if skills.is_empty() {
        vec![one(None)]
    } else {
        skills.iter().map(|s| one(Some(s))).collect()
    })
}

/// One `--json remove <skill>|--all --yes --global|--project
/// [--agent <name>]` per skill and agent (upstream takes one of each).
pub fn plan_remove(req: &RemoveRequest) -> Result<Vec<Plan>> {
    if req.skills.is_empty() {
        return Err(MarketplaceError::Invalid(
            "remove needs at least one skill name".to_string(),
        ));
    }
    let all = checked_names(&req.skills)?;
    let agents: Vec<Option<String>> = match agent_names(&req.agents)? {
        list if list.is_empty() => vec![None],
        list => list.into_iter().map(Some).collect(),
    };
    let targets: Vec<&str> = if all {
        vec!["--all"]
    } else {
        req.skills.iter().map(String::as_str).collect()
    };
    let mut plans = Vec::new();
    for target in targets {
        for agent in &agents {
            let mut plan = json_plan("remove", Some(&req.scope));
            plan.extend([target, "--yes", location_flag(&req.scope)]);
            if let Some(agent) = agent {
                plan.extend(["--agent", agent]);
            }
            plans.push(plan);
        }
    }
    Ok(plans)
}

/// Re-install one outdated skill from its recorded source, as
/// `skillfish update` itself does: `--json add <owner/repo> --path <path>
/// --force --yes --global|--project --agent <agent>`, or by skill name
/// when the manifest has no path.
pub fn plan_reinstall(outdated: &OutdatedSkill, scope: &Scope) -> Result<Plan> {
    let (owner, repo) = outdated.source.split_once('/').ok_or_else(|| {
        MarketplaceError::Parse(format!("unrecognised source {:?}", outdated.source))
    })?;
    check(Kind::Name, owner)?;
    check(Kind::Name, repo)?;
    let mut plan = json_plan("add", Some(scope));
    plan.push(&outdated.source);
    match read_skillfish_manifest(&outdated.path).and_then(|m| m.path) {
        Some(path) => {
            check(Kind::Path, &path)?;
            plan.extend(["--path", &path]);
        }
        None => {
            check(Kind::Name, &outdated.skill)?;
            plan.push(&outdated.skill);
        }
    }
    plan.extend(["--force", "--yes", location_flag(scope)]);
    if !outdated.agent.is_empty() {
        plan.extend(["--agent", &outdated.agent]);
    }
    Ok(plan)
}

/// The skillfish CLI as a [`Marketplace`].
pub struct Skillfish<R> {
    cli: Cli<R>,
    home: PathBuf,
}

impl<R: CommandRunner> Skillfish<R> {
    /// Runs `npx --yes skillfish …`; `home` is the user's HOME (global
    /// updates run there).
    pub fn new(runner: R, home: PathBuf) -> Self {
        Self::with_command(runner, home, "npx", &["--yes", "skillfish"])
    }

    /// Runs `<program> <prefix…> …`, e.g. a pinned `npx --yes
    /// skillfish@1.4.0` or an installed `skillfish` binary.
    pub fn with_command(runner: R, home: PathBuf, program: &str, prefix: &[&str]) -> Self {
        Self {
            cli: Cli::new(runner, program, prefix),
            home,
        }
    }

    /// Every command shares `cancel`.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cli.cancel = cancel;
        self
    }

    /// Run `update --json` for `scope`; the output is kept even on a
    /// non-zero exit (the JSON still lists what was confirmed).
    fn check_report(&self, scope: &Scope) -> Result<(Output, UpdateReport)> {
        let output = self.cli.run(plan_check(scope, &self.home), CHECK_TIMEOUT)?;
        match parse_update(&output.stdout) {
            Ok(report) => Ok((output, report)),
            Err(_) if !output.success() => Err(failed(&output)),
            Err(e) => Err(MarketplaceError::Parse(e)),
        }
    }

    /// Run one mutating plan and fold its JSON rows into `outcome`.
    /// `Err` only when nothing at all was confirmed and the command failed.
    fn mutate(&self, plan: Plan, sub: &str, outcome: &mut Outcome) -> Result<()> {
        let output = self.cli.run(plan, MUTATE_TIMEOUT)?;
        let Some(env) = parse::envelope(&output.stdout) else {
            if !output.success() {
                return Err(failed(&output));
            }
            outcome.warnings.push(format!(
                "unrecognised `{}` output; changes not confirmed",
                self.cli.label(sub)
            ));
            return Ok(());
        };
        let rows = if sub == "remove" {
            &env.removed
        } else {
            &env.installed
        };
        let changed = parse::names(rows);
        for s in &env.skipped {
            outcome
                .warnings
                .push(format!("{} ({}): {}", s.skill, s.agent, s.reason));
        }
        if !output.success() || !env.success {
            if changed.is_empty() && !output.success() {
                return Err(failed_with(&output, &env.errors.join("; ")));
            }
            outcome.warnings.extend(env.errors.iter().cloned());
            if !output.success() {
                outcome
                    .warnings
                    .push(exit_warning(&self.cli.label(sub), &output));
            }
        }
        for name in changed {
            push_unique(&mut outcome.changed, &name);
        }
        Ok(())
    }

    fn in_scope(scope: &Scope, location: &str) -> bool {
        match scope {
            // Global runs from HOME, where both locations are HOME's.
            Scope::Global => true,
            Scope::Project(_) => location == "project",
        }
    }
}

impl<R: CommandRunner> Marketplace for Skillfish<R> {
    fn id(&self) -> ChannelId {
        ChannelId::Skillfish
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            search: true,
            install: true,
            remove: true,
            update: true,
            check: true,
        }
    }

    fn search(&self, query: &str) -> Result<SearchResults> {
        let output = self.cli.run_ok(plan_search(query)?, SEARCH_TIMEOUT)?;
        parse_search(&output.stdout).map_err(MarketplaceError::Parse)
    }

    fn list(&self, scope: &Scope) -> Result<Vec<InstalledSkill>> {
        let output = self.cli.run_ok(plan_list(scope), LIST_TIMEOUT)?;
        parse_list(&output.stdout, scope).map_err(MarketplaceError::Parse)
    }

    /// One `update --json` per scope. Outdated entries are `Some(true)`;
    /// after a clean run every other tracked skill is `Some(false)`.
    /// Skills named in an error, manifest-controlled skills, skills without
    /// a manifest, and everything after a failed run are `None`.
    fn check(&self, installed: &[InstalledSkill]) -> Result<Vec<UpdateInfo>> {
        let mut scopes: Vec<&Scope> = Vec::new();
        for skill in installed {
            if !scopes.contains(&&skill.scope) {
                scopes.push(&skill.scope);
            }
        }
        let mut reports = Vec::new();
        for scope in scopes {
            reports.push((scope, self.check_report(scope)?));
        }
        Ok(installed
            .iter()
            .map(|skill| {
                let (_, (output, report)) = reports
                    .iter()
                    .find(|(scope, _)| **scope == skill.scope)
                    .expect("every scope was checked");
                let outdated = report.outdated.iter().any(|o| {
                    o.path == skill.path
                        || (o.skill == skill.name && Self::in_scope(&skill.scope, &o.location))
                });
                let errored = report
                    .errors
                    .iter()
                    .any(|e| e.starts_with(&format!("{}:", skill.name)));
                let tracked = read_skillfish_manifest(&skill.path)
                    .is_some_and(|m| m.source.as_deref() != Some("manifest"));
                let available = if outdated {
                    Some(true)
                } else if output.success() && tracked && !errored {
                    Some(false)
                } else {
                    None
                };
                UpdateInfo {
                    name: skill.name.clone(),
                    available,
                }
            })
            .collect())
    }

    fn install(&self, req: &InstallRequest) -> Result<Outcome> {
        let mut outcome = Outcome::default();
        for plan in plan_install(req)? {
            self.mutate(plan, "add", &mut outcome)?;
        }
        Ok(outcome)
    }

    fn remove(&self, req: &RemoveRequest) -> Result<Outcome> {
        let mut outcome = Outcome::default();
        for plan in plan_remove(req)? {
            self.mutate(plan, "remove", &mut outcome)?;
        }
        Ok(outcome)
    }

    /// No names: `update --yes --json`, changed = its `updated` rows.
    /// Names: check first, then re-install only those reported outdated.
    fn update(&self, req: &UpdateRequest) -> Result<Outcome> {
        req.skills.iter().try_for_each(|s| check(Kind::Name, s))?;
        if req.skills.is_empty() {
            let output = self
                .cli
                .run(plan_update_all(&req.scope, &self.home), MUTATE_TIMEOUT)?;
            let report = match parse_update(&output.stdout) {
                Ok(report) => report,
                Err(_) if !output.success() => return Err(failed(&output)),
                Err(e) => return Err(MarketplaceError::Parse(e)),
            };
            let rows: Vec<Row> = report
                .updated
                .into_iter()
                .filter(|r| Self::in_scope(&req.scope, r.location.as_deref().unwrap_or("")))
                .collect();
            let changed = parse::names(&rows);
            if !output.success() && changed.is_empty() {
                return Err(failed_with(&output, &report.errors.join("; ")));
            }
            let mut warnings = report.errors;
            if !output.success() {
                warnings.push(exit_warning(&self.cli.label("update"), &output));
            }
            return Ok(Outcome { changed, warnings });
        }
        let (_, report) = self.check_report(&req.scope)?;
        let mut outcome = Outcome {
            changed: vec![],
            warnings: report.errors.clone(),
        };
        for outdated in report.outdated.iter().filter(|o| {
            req.skills.iter().any(|s| s.eq_ignore_ascii_case(&o.skill))
                && Self::in_scope(&req.scope, &o.location)
        }) {
            self.mutate(plan_reinstall(outdated, &req.scope)?, "add", &mut outcome)?;
        }
        Ok(outcome)
    }
}
