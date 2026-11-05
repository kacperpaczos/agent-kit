//! Adapter for vercel-labs/skills (`npx skills`).
//!
//! Every operation is a pure `plan_*` function building the argv after the
//! `skills` word, then one run through the injected [`CommandRunner`]. The
//! CLI owns its lock files (`~/.agents/.skill-lock.json` or
//! `$XDG_STATE_HOME/skills/.skill-lock.json`, project `skills-lock.json`);
//! this adapter never writes them.
//!
//! The CLI collects `--agent`/`--skill` values greedily until the next
//! argument starting with `-`, so positional names always come before
//! those flags and the agent list comes last.

mod parse;

use std::path::PathBuf;
use std::time::Duration;

pub use parse::{parse_add_json, parse_check, parse_find, parse_list_json};

use crate::runner::{Command, CommandRunner, Output};
use crate::types::{
    AgentSelection, Capabilities, ChannelId, InstallRequest, InstalledSkill, Marketplace,
    MarketplaceError, Outcome, RemoveRequest, Result, Scope, SearchResults, SourceKind, UpdateInfo,
    UpdateRequest,
};
use crate::validate::{Kind, check, check_name_or_all};

/// One agent known to both sides: its agent-kit id, its `npx skills`
/// agent name (`-a`), and the display name `list --json` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentName {
    pub id: &'static str,
    pub npx: &'static str,
    pub display: &'static str,
}

/// From vercel-labs/skills `src/agents.ts`.
pub const AGENT_NAMES: &[AgentName] = &[
    AgentName {
        id: "claude",
        npx: "claude-code",
        display: "Claude Code",
    },
    AgentName {
        id: "codex",
        npx: "codex",
        display: "Codex",
    },
    AgentName {
        id: "opencode",
        npx: "opencode",
        display: "OpenCode",
    },
];

/// `npx skills` agent name for an agent-kit id.
pub fn npx_agent_name(id: &str) -> Option<&'static str> {
    AGENT_NAMES.iter().find(|a| a.id == id).map(|a| a.npx)
}

/// Agent-kit id for a display name from `list --json`.
pub fn agent_id_for_display(display: &str) -> Option<&'static str> {
    AGENT_NAMES
        .iter()
        .find(|a| a.display == display)
        .map(|a| a.id)
}

const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MUTATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Argv after the `skills` word, plus the directory to run in (the project
/// for project scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl Plan {
    fn new(sub: &str, scope: Option<&Scope>) -> Plan {
        Plan {
            args: vec![sub.to_string()],
            cwd: match scope {
                Some(Scope::Project(dir)) => Some(dir.clone()),
                _ => None,
            },
        }
    }
    fn push(&mut self, arg: &str) {
        self.args.push(arg.to_string());
    }
    fn extend<'a>(&mut self, args: impl IntoIterator<Item = &'a str>) {
        self.args.extend(args.into_iter().map(str::to_string));
    }
}

/// `find <word>...` — the CLI joins the words back with spaces.
pub fn plan_search(query: &str) -> Result<Plan> {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Err(MarketplaceError::Invalid("empty search query".to_string()));
    }
    for word in &words {
        check(Kind::Word, word)?;
    }
    let mut plan = Plan::new("find", None);
    plan.extend(words);
    Ok(plan)
}

/// `list [-g] --json`.
pub fn plan_list(scope: &Scope) -> Plan {
    let mut plan = Plan::new("list", Some(scope));
    if *scope == Scope::Global {
        plan.push("-g");
    }
    plan.push("--json");
    plan
}

/// `check [name...] -g|-p`; exits 1 when an update exists.
pub fn plan_check(scope: &Scope, names: &[String]) -> Result<Plan> {
    names.iter().try_for_each(|n| check(Kind::Name, n))?;
    let mut plan = Plan::new("check", Some(scope));
    plan.extend(names.iter().map(String::as_str));
    plan.push(scope_only_flag(scope));
    Ok(plan)
}

/// `add <source> [-g] -y --json [--skill <name>...] [--agent <agent>...]`.
pub fn plan_install(req: &InstallRequest) -> Result<Plan> {
    let source = source_arg(&req.source.kind)?;
    if let Some(skill) = &req.source.skill {
        check(Kind::Name, skill)?;
    }
    let skills: Vec<&str> = if req.skills.is_empty() {
        req.source.skill.iter().map(String::as_str).collect()
    } else {
        req.skills.iter().map(String::as_str).collect()
    };
    skills.iter().try_for_each(|s| check_name_or_all(s))?;
    let agents = agent_args(&req.agents)?;
    let mut plan = Plan::new("add", Some(&req.scope));
    plan.push(&source);
    if req.scope == Scope::Global {
        plan.push("-g");
    }
    plan.extend(["-y", "--json"]);
    if !skills.is_empty() {
        plan.push("--skill");
        plan.extend(skills);
    }
    plan.args.extend(agents);
    Ok(plan)
}

/// `remove <name>... [-g] -y [--agent <agent>...]`, or `remove --skill *`.
pub fn plan_remove(req: &RemoveRequest) -> Result<Plan> {
    if req.skills.is_empty() {
        return Err(MarketplaceError::Invalid(
            "remove needs at least one skill name".to_string(),
        ));
    }
    let all = req.skills.iter().any(|s| s == "*");
    if all && req.skills.len() > 1 {
        return Err(MarketplaceError::Invalid(
            "'*' cannot be combined with skill names".to_string(),
        ));
    }
    if !all {
        req.skills.iter().try_for_each(|s| check(Kind::Name, s))?;
    }
    let agents = agent_args(&req.agents)?;
    let mut plan = Plan::new("remove", Some(&req.scope));
    if all {
        plan.extend(["--skill", "*"]);
    } else {
        plan.extend(req.skills.iter().map(String::as_str));
    }
    if req.scope == Scope::Global {
        plan.push("-g");
    }
    plan.push("-y");
    plan.args.extend(agents);
    Ok(plan)
}

/// `update [name...] -g|-p -y`.
pub fn plan_update(req: &UpdateRequest) -> Result<Plan> {
    req.skills.iter().try_for_each(|s| check(Kind::Name, s))?;
    let mut plan = Plan::new("update", Some(&req.scope));
    plan.extend(req.skills.iter().map(String::as_str));
    plan.extend([scope_only_flag(&req.scope), "-y"]);
    Ok(plan)
}

/// `update`/`check` restrict scope with `-g` or `-p`.
fn scope_only_flag(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "-g",
        Scope::Project(_) => "-p",
    }
}

fn source_arg(kind: &SourceKind) -> Result<String> {
    match kind {
        SourceKind::Github { owner, repo } => {
            check(Kind::Name, owner)?;
            check(Kind::Name, repo)?;
            Ok(format!("{owner}/{repo}"))
        }
        SourceKind::Url { url } => {
            check(Kind::Url, url)?;
            Ok(url.clone())
        }
        SourceKind::Local { path } => {
            let text = path.to_str().ok_or_else(|| {
                MarketplaceError::Invalid(format!("path {path:?} is not valid UTF-8"))
            })?;
            check(Kind::Path, text)?;
            Ok(text.to_string())
        }
        SourceKind::Plugin { .. } => Err(MarketplaceError::Unsupported(
            "npx skills does not install plugins".to_string(),
        )),
    }
}

/// `--agent` and its values; agent-kit ids become `npx skills` names,
/// other names pass through.
fn agent_args(agents: &AgentSelection) -> Result<Vec<String>> {
    match agents {
        AgentSelection::Detected => Ok(vec![]),
        AgentSelection::All => Ok(vec!["--agent".to_string(), "*".to_string()]),
        AgentSelection::Only(list) if list.is_empty() => {
            Err(MarketplaceError::Invalid("empty agent list".to_string()))
        }
        AgentSelection::Only(list) => {
            let mut args = vec!["--agent".to_string()];
            for agent in list {
                check(Kind::Name, agent)?;
                args.push(npx_agent_name(agent).unwrap_or(agent).to_string());
            }
            Ok(args)
        }
    }
}

/// The vercel-labs/skills CLI as a [`Marketplace`].
pub struct NpxSkills<R> {
    runner: R,
    program: String,
    prefix: Vec<String>,
}

impl<R: CommandRunner> NpxSkills<R> {
    /// Runs `npx --yes skills …` (`--yes` so npx never stops to ask
    /// whether to download the package).
    pub fn new(runner: R) -> Self {
        Self::with_command(runner, "npx", &["--yes", "skills"])
    }

    /// Runs `<program> <prefix…> <subcommand…>`, e.g. a pinned
    /// `npx --yes skills@1.8.0` or an installed `skills` binary.
    pub fn with_command(runner: R, program: &str, prefix: &[&str]) -> Self {
        Self {
            runner,
            program: program.to_string(),
            prefix: prefix.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn run(&self, plan: Plan, timeout: Duration) -> Result<Output> {
        let mut args = self.prefix.clone();
        args.extend(plan.args);
        let cmd = Command {
            program: self.program.clone(),
            args,
            cwd: plan.cwd,
            env: vec![],
            timeout,
        };
        self.runner.run(&cmd).map_err(MarketplaceError::Run)
    }

    /// Run and require exit code 0.
    fn run_ok(&self, plan: Plan, timeout: Duration) -> Result<Output> {
        let output = self.run(plan, timeout)?;
        if output.success() {
            Ok(output)
        } else {
            Err(failed(&output))
        }
    }
}

fn failed(output: &Output) -> MarketplaceError {
    MarketplaceError::Failed {
        status: output.status,
        stderr: output.stderr.trim().to_string(),
    }
}

impl<R: CommandRunner> Marketplace for NpxSkills<R> {
    fn id(&self) -> ChannelId {
        ChannelId::NpxSkills
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
        let output = self.run_ok(plan_search(query)?, SEARCH_TIMEOUT)?;
        Ok(parse_find(&output.stdout))
    }

    fn list(&self, scope: &Scope) -> Result<Vec<InstalledSkill>> {
        let output = self.run_ok(plan_list(scope), LIST_TIMEOUT)?;
        parse_list_json(&output.stdout, scope).map_err(MarketplaceError::Parse)
    }

    fn check(&self, installed: &[InstalledSkill]) -> Result<Vec<UpdateInfo>> {
        // One `check` per scope, in order of first appearance.
        let mut groups: Vec<(&Scope, Vec<usize>)> = Vec::new();
        for (i, skill) in installed.iter().enumerate() {
            match groups.iter_mut().find(|(scope, _)| **scope == skill.scope) {
                Some((_, indices)) => indices.push(i),
                None => groups.push((&skill.scope, vec![i])),
            }
        }
        let mut infos: Vec<Option<UpdateInfo>> = vec![None; installed.len()];
        for (scope, indices) in groups {
            let names: Vec<String> = indices.iter().map(|&i| installed[i].name.clone()).collect();
            let output = self.run(plan_check(scope, &names)?, CHECK_TIMEOUT)?;
            for (&i, info) in indices.iter().zip(parse_check(&output, &names)) {
                infos[i] = Some(info);
            }
        }
        Ok(infos.into_iter().flatten().collect())
    }

    fn install(&self, req: &InstallRequest) -> Result<Outcome> {
        let output = self.run(plan_install(req)?, MUTATE_TIMEOUT)?;
        match parse_add_json(&output.stdout) {
            Some(outcome) if output.success() || !outcome.changed.is_empty() => Ok(outcome),
            Some(_) | None if !output.success() => Err(failed(&output)),
            _ => {
                let mut changed = req.skills.clone();
                if changed.is_empty() {
                    changed.extend(req.source.skill.clone());
                }
                Ok(Outcome {
                    changed,
                    warnings: vec![
                        "unrecognised `skills add` output; assuming the requested skills were installed"
                            .to_string(),
                    ],
                })
            }
        }
    }

    fn remove(&self, req: &RemoveRequest) -> Result<Outcome> {
        self.run_ok(plan_remove(req)?, MUTATE_TIMEOUT)?;
        Ok(Outcome {
            changed: req.skills.clone(),
            warnings: vec![],
        })
    }

    fn update(&self, req: &UpdateRequest) -> Result<Outcome> {
        let output = self.run_ok(plan_update(req)?, MUTATE_TIMEOUT)?;
        let changed = parse::parse_updated(&output.stdout);
        if changed.is_empty() && !req.skills.is_empty() {
            return Ok(Outcome {
                changed: req.skills.clone(),
                warnings: vec![
                    "no per-skill result in `skills update` output; reporting the requested skills"
                        .to_string(),
                ],
            });
        }
        Ok(Outcome {
            changed,
            warnings: vec![],
        })
    }
}
