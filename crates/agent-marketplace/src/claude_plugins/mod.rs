//! Adapter for Claude Code plugin marketplaces (`claude plugin …`).
//!
//! Facts from code.claude.com/docs/en/plugins/cli-reference:
//!
//! - `plugin list --json` prints one object per installation (`id` is
//!   `name@marketplace`, plus `version`, `scope`, `enabled`, `installPath`,
//!   optional `readFromFolder`, `projectPath`, `lastUpdated`);
//!   `--json --available` wraps it as `{installed, available}`, where each
//!   available entry has `pluginId`, `name`, `marketplaceName`, `source`
//!   (a string for a relative path), optional `description`, `version`,
//!   `installCount`.
//! - `plugin marketplace list --json`: `name`, `source`, `installLocation`.
//! - `plugin install|uninstall|update <plugin> [-s|--scope <scope>]
//!   [--json]` print one JSON result line last on stdout (`command`,
//!   `outcome` `ok`/`failed`, `message`); exit 0 or 1. Scopes: `user`,
//!   `project`, `local` (`update` also `managed`).
//!
//! Plugins are installed, removed and updated whole. This adapter never
//! passes `-y`: a plugin whose marketplace runs an install command is
//! refused without a terminal and comes back as a failure for the user to
//! accept in Claude Code. The CLI cannot check for updates without
//! applying them, so `check` reports `None`. Changes are confirmed by
//! comparing `plugin list --json` before and after.

mod parse;

use std::path::Path;

pub use parse::{
    AvailablePlugin, KnownMarketplace, PluginInstall, ResultLine, parse_available,
    parse_marketplaces, parse_plugin_list, parse_result_line,
};

use crate::exec::{
    Cli, LIST_TIMEOUT, MUTATE_TIMEOUT, Plan, SEARCH_TIMEOUT, failed_with, matches_query,
    push_unique, query_words, run_each,
};
use crate::provenance::plugin_skill_dirs;
use crate::runner::{CancelToken, CommandRunner};
use crate::types::{
    Capabilities, ChannelId, InstallRequest, InstalledSkill, Marketplace, MarketplaceError,
    Outcome, RemoveRequest, Result, Scope, SearchHit, SearchResults, SourceKind, SourceRef,
    UpdateInfo, UpdateRequest,
};
use crate::validate::{Kind, check};

const SCOPES: &[&str] = &["user", "project", "local", "managed"];

/// `list --json`; runs in the project for project scope.
pub fn plan_list(scope: &Scope) -> Plan {
    let mut plan = Plan::new("list", Some(scope));
    plan.push("--json");
    plan
}

/// `list --json --available`.
pub fn plan_available() -> Plan {
    let mut plan = Plan::new("list", None);
    plan.extend(["--json", "--available"]);
    plan
}

/// `marketplace list --json`.
pub fn plan_marketplaces() -> Plan {
    let mut plan = Plan::new("marketplace", None);
    plan.extend(["list", "--json"]);
    plan
}

/// A plugin or marketplace name: a plain name without `@`.
fn check_segment(value: &str) -> Result<()> {
    check(Kind::Name, value)?;
    if value.contains('@') {
        return Err(MarketplaceError::Invalid(format!(
            "name {value:?}: contains '@'"
        )));
    }
    Ok(())
}

/// `name` or `name@marketplace`.
fn check_plugin_id(id: &str) -> Result<()> {
    match id.split_once('@') {
        Some((name, market)) => {
            check_segment(name)?;
            check_segment(market)
        }
        None => check_segment(id),
    }
}

fn scope_name(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "user",
        Scope::Project(_) => "project",
    }
}

fn scoped(sub: &str, id: &str, scope: &str, project: Option<&Path>) -> Result<Plan> {
    check_plugin_id(id)?;
    if !SCOPES.contains(&scope) {
        return Err(MarketplaceError::Invalid(format!(
            "unknown scope {scope:?}"
        )));
    }
    let mut plan = Plan::new(sub, None).in_dir(project);
    plan.extend([id, "--scope", scope, "--json"]);
    Ok(plan)
}

/// `install <plugin>@<marketplace> --scope user|project --json`.
pub fn plan_install(req: &InstallRequest) -> Result<Plan> {
    let SourceKind::Plugin {
        marketplace,
        plugin,
    } = &req.source.kind
    else {
        return Err(MarketplaceError::Unsupported(
            "Claude Code installs plugins from its marketplaces only".to_string(),
        ));
    };
    check_segment(marketplace)?;
    check_segment(plugin)?;
    let project = match &req.scope {
        Scope::Project(dir) => Some(dir.as_path()),
        Scope::Global => None,
    };
    scoped(
        "install",
        &format!("{plugin}@{marketplace}"),
        scope_name(&req.scope),
        project,
    )
}

/// `uninstall <plugin> --scope <scope> --json`.
pub fn plan_uninstall(id: &str, scope: &str, project: Option<&Path>) -> Result<Plan> {
    if scope == "managed" {
        return Err(MarketplaceError::Unsupported(
            "managed plugins are removed by an administrator".to_string(),
        ));
    }
    scoped("uninstall", id, scope, project)
}

/// `update <plugin> --scope <scope> --json`.
pub fn plan_update(id: &str, scope: &str, project: Option<&Path>) -> Result<Plan> {
    scoped("update", id, scope, project)
}

/// The record matches `scope`: user/managed for global, project/local of
/// that project otherwise. Session, skills-dir and synced pseudo-installs
/// are not marketplace plugins.
fn in_scope(p: &PluginInstall, scope: &Scope) -> bool {
    if matches!(p.marketplace.as_str(), "inline" | "skills-dir") {
        return false;
    }
    match scope {
        Scope::Global => matches!(p.scope.as_str(), "user" | "managed" | "synced"),
        Scope::Project(dir) => {
            matches!(p.scope.as_str(), "project" | "local")
                && p.project_path.as_ref().is_none_or(|pp| pp == dir)
        }
    }
}

/// Ids that appeared (`added`), vanished (`removed`) or whose version,
/// path or update time changed between two listings, restricted to scope.
struct Diff {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

fn diff(before: &[PluginInstall], after: &[PluginInstall], scope: &Scope) -> Diff {
    let key = |p: &PluginInstall| (p.id.clone(), p.scope.clone());
    let before: Vec<&PluginInstall> = before.iter().filter(|p| in_scope(p, scope)).collect();
    let after: Vec<&PluginInstall> = after.iter().filter(|p| in_scope(p, scope)).collect();
    let mut d = Diff {
        added: vec![],
        removed: vec![],
        changed: vec![],
    };
    for a in &after {
        match before.iter().find(|b| key(b) == key(a)) {
            None => push_unique(&mut d.added, &a.id),
            Some(b) => {
                if (&b.version, &b.root, &b.last_updated) != (&a.version, &a.root, &a.last_updated)
                {
                    push_unique(&mut d.changed, &a.id);
                }
            }
        }
    }
    for b in &before {
        if !after.iter().any(|a| key(a) == key(b)) {
            push_unique(&mut d.removed, &b.id);
        }
    }
    d
}

/// Claude Code's plugin commands as a [`Marketplace`].
pub struct ClaudePlugins<R> {
    cli: Cli<R>,
}

impl<R: CommandRunner> ClaudePlugins<R> {
    /// Runs `claude plugin …`.
    pub fn new(runner: R) -> Self {
        Self::with_command(runner, "claude", &["plugin"])
    }

    /// Runs `<program> <prefix…> …`.
    pub fn with_command(runner: R, program: &str, prefix: &[&str]) -> Self {
        Self {
            cli: Cli::new(runner, program, prefix),
        }
    }

    /// Every command shares `cancel`.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cli.cancel = cancel;
        self
    }

    /// Installed plugin records (all scopes the CLI reports from `scope`'s
    /// directory).
    pub fn plugins(&self, scope: &Scope) -> Result<Vec<PluginInstall>> {
        let output = self.cli.run_ok(plan_list(scope), LIST_TIMEOUT)?;
        parse_plugin_list(&output.stdout).map_err(MarketplaceError::Parse)
    }

    /// Run `plan`, then compare listings. `pick` selects the confirmed ids
    /// from the diff. A failed command with nothing confirmed is an error
    /// carrying the JSON result message.
    fn mutate(
        &self,
        plan: Plan,
        scope: &Scope,
        before: &[PluginInstall],
        pick: fn(Diff) -> Vec<String>,
        outcome: &mut Outcome,
    ) -> Result<()> {
        let output = self.cli.run(plan, MUTATE_TIMEOUT)?;
        let line = parse_result_line(&output.stdout);
        let ok = output.success() && line.as_ref().is_none_or(|l| l.outcome == "ok");
        let confirmed = match self.plugins(scope) {
            Ok(after) => pick(diff(before, &after, scope)),
            Err(e) => {
                outcome
                    .warnings
                    .push(format!("change not confirmed: listing failed ({e})"));
                vec![]
            }
        };
        let message = line.map(|l| l.message).unwrap_or_default();
        if !ok && confirmed.is_empty() {
            return Err(failed_with(&output, &message));
        }
        if confirmed.is_empty() && !message.is_empty() {
            outcome.warnings.push(message);
        } else if !ok {
            outcome.warnings.push(format!("command failed: {message}"));
        }
        for id in confirmed {
            push_unique(&mut outcome.changed, &id);
        }
        Ok(())
    }

    /// Where `id` (bare or qualified) is installed for `scope`.
    fn find<'a>(before: &'a [PluginInstall], id: &str, scope: &Scope) -> Option<&'a PluginInstall> {
        before
            .iter()
            .filter(|p| in_scope(p, scope))
            .find(|p| p.id == id || (!id.contains('@') && p.plugin == id))
    }

    fn project_dir(scope: &Scope) -> Option<&Path> {
        match scope {
            Scope::Project(dir) => Some(dir.as_path()),
            Scope::Global => None,
        }
    }
}

impl<R: CommandRunner> Marketplace for ClaudePlugins<R> {
    fn id(&self) -> ChannelId {
        ChannelId::ClaudePlugins
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            search: true,
            install: true,
            remove: true,
            update: true,
            check: false,
        }
    }

    /// Plugins the configured marketplaces offer (not installed) whose name
    /// or description holds every query word, each followed by the skills
    /// it contains when its marketplace copy is on disk; a plugin that does
    /// not match still yields its matching skills.
    fn search(&self, query: &str) -> Result<SearchResults> {
        let words = query_words(query)?;
        words.iter().try_for_each(|w| check(Kind::Word, w))?;
        let output = self.cli.run_ok(plan_available(), SEARCH_TIMEOUT)?;
        let available = parse_available(&output.stdout).map_err(MarketplaceError::Parse)?;
        let mut results = SearchResults::default();
        let markets = match self.cli.run_ok(plan_marketplaces(), LIST_TIMEOUT) {
            Ok(out) => parse_marketplaces(&out.stdout).unwrap_or_else(|e| {
                results.warnings.push(e);
                vec![]
            }),
            Err(e) => {
                results.warnings.push(format!(
                    "plugin skills unknown: marketplace list failed ({e})"
                ));
                vec![]
            }
        };
        for plugin in available {
            let source = |skill: Option<&str>| SourceRef {
                kind: SourceKind::Plugin {
                    marketplace: plugin.marketplace.clone(),
                    plugin: plugin.name.clone(),
                },
                skill: skill.map(str::to_string),
            };
            let skills = match (
                &plugin.source_path,
                markets.iter().find(|m| m.name == plugin.marketplace),
            ) {
                (
                    Some(rel),
                    Some(KnownMarketplace {
                        install_location: Some(dir),
                        ..
                    }),
                ) => plugin_skill_dirs(&dir.join(rel), &plugin.name),
                _ => vec![],
            };
            let plugin_hit =
                matches_query(&words, &[Some(&plugin.name), plugin.description.as_deref()]);
            if plugin_hit {
                results.hits.push(SearchHit {
                    name: plugin.name.clone(),
                    source: source(None),
                    description: plugin.description.clone(),
                    installs: plugin.install_count,
                    url: None,
                });
            }
            for (skill, _) in skills {
                if plugin_hit || matches_query(&words, &[Some(&skill)]) {
                    results.hits.push(SearchHit {
                        source: source(Some(&skill)),
                        name: skill,
                        description: None,
                        installs: None,
                        url: None,
                    });
                }
            }
        }
        Ok(results)
    }

    /// Each skill inside the marketplace plugins installed for `scope`.
    fn list(&self, scope: &Scope) -> Result<Vec<InstalledSkill>> {
        let plugins = self.plugins(scope)?;
        Ok(plugins
            .iter()
            .filter(|p| in_scope(p, scope))
            .flat_map(|p| {
                let skills = p
                    .root
                    .as_deref()
                    .map(|root| plugin_skill_dirs(root, &p.plugin))
                    .unwrap_or_default();
                skills.into_iter().map(move |(name, path)| InstalledSkill {
                    source: Some(SourceRef {
                        kind: SourceKind::Plugin {
                            marketplace: p.marketplace.clone(),
                            plugin: p.plugin.clone(),
                        },
                        skill: Some(name.clone()),
                    }),
                    name,
                    path,
                    agents: vec!["claude".to_string()],
                    scope: scope.clone(),
                    channel: ChannelId::ClaudePlugins,
                })
            })
            .collect())
    }

    /// Unknown: `claude plugin` cannot check without updating.
    fn check(&self, installed: &[InstalledSkill]) -> Result<Vec<UpdateInfo>> {
        Ok(installed
            .iter()
            .map(|s| UpdateInfo {
                name: s.name.clone(),
                available: None,
            })
            .collect())
    }

    /// Installs the whole plugin; `changed` holds the plugin ids that
    /// appeared (dependencies included).
    fn install(&self, req: &InstallRequest) -> Result<Outcome> {
        let plan = plan_install(req)?;
        let before = self.plugins(&req.scope)?;
        let mut outcome = Outcome::default();
        if !req.skills.is_empty() || req.source.skill.is_some() {
            outcome.warnings.push(
                "Claude Code installs the whole plugin; skill selection is ignored".to_string(),
            );
        }
        self.mutate(
            plan,
            &req.scope,
            &before,
            |d| [d.added, d.changed].concat(),
            &mut outcome,
        )?;
        Ok(outcome)
    }

    /// `skills` are plugin ids (`name` or `name@marketplace`); each is
    /// uninstalled from the scope it is installed at.
    fn remove(&self, req: &RemoveRequest) -> Result<Outcome> {
        if req.skills.is_empty() {
            return Err(MarketplaceError::Invalid(
                "remove needs at least one plugin".to_string(),
            ));
        }
        if req.skills.iter().any(|s| s == "*") {
            return Err(MarketplaceError::Unsupported(
                "Claude Code uninstalls plugins one at a time".to_string(),
            ));
        }
        req.skills.iter().try_for_each(|s| check_plugin_id(s))?;
        let mut outcome = Outcome::default();
        run_each(&req.skills, &mut outcome, |id, out| {
            let before = self.plugins(&req.scope)?;
            let (id, scope) = match Self::find(&before, id, &req.scope) {
                Some(p) => (p.id.clone(), p.scope.clone()),
                None => (id.clone(), scope_name(&req.scope).to_string()),
            };
            let plan = plan_uninstall(&id, &scope, Self::project_dir(&req.scope))?;
            self.mutate(plan, &req.scope, &before, |d| d.removed, out)
        })?;
        Ok(outcome)
    }

    /// `skills` are plugin ids; empty updates every plugin in the scope.
    fn update(&self, req: &UpdateRequest) -> Result<Outcome> {
        req.skills.iter().try_for_each(|s| check_plugin_id(s))?;
        let before = self.plugins(&req.scope)?;
        let targets: Vec<(String, String)> = if req.skills.is_empty() {
            before
                .iter()
                .filter(|p| in_scope(p, &req.scope) && p.scope != "synced")
                .map(|p| (p.id.clone(), p.scope.clone()))
                .collect()
        } else {
            req.skills
                .iter()
                .map(|id| match Self::find(&before, id, &req.scope) {
                    Some(p) => (p.id.clone(), p.scope.clone()),
                    None => (id.clone(), scope_name(&req.scope).to_string()),
                })
                .collect()
        };
        let project = Self::project_dir(&req.scope);
        let mut outcome = Outcome::default();
        let mut failures = Vec::new();
        for (id, scope) in &targets {
            let output = self
                .cli
                .run(plan_update(id, scope, project)?, MUTATE_TIMEOUT)?;
            let line = parse_result_line(&output.stdout);
            if !output.success() || line.as_ref().is_some_and(|l| l.outcome != "ok") {
                let msg = line
                    .map(|l| l.message)
                    .unwrap_or_else(|| output.stderr.trim().to_string());
                failures.push((output, format!("{id}: {msg}")));
            }
        }
        match self.plugins(&req.scope) {
            Ok(after) => outcome.changed = diff(&before, &after, &req.scope).changed,
            Err(e) => outcome
                .warnings
                .push(format!("update not confirmed: listing failed ({e})")),
        }
        if outcome.changed.is_empty()
            && let Some((output, _)) = failures.first()
            && failures.len() == targets.len()
        {
            let all: Vec<&str> = failures.iter().map(|(_, m)| m.as_str()).collect();
            return Err(failed_with(output, &all.join("; ")));
        }
        outcome
            .warnings
            .extend(failures.into_iter().map(|(_, m)| m));
        Ok(outcome)
    }
}
